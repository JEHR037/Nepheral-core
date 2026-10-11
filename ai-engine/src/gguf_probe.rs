// SPDX-License-Identifier: BUSL-1.1
//! Reading enough of a GGUF header to explain why llama.cpp refused it.
//!
//! llama.cpp's own log says why it refused a file, and that is what the
//! loader quotes. This adds the one thing the log does not always name: the
//! architecture the file declares, read from the header it carries. It is also
//! what tells a real GGUF from an arbitrary file of the right size, which is
//! how a model arriving over the mesh is checked.
//!
//! This describes, it does not judge. What a build can run is decided by the
//! llama.cpp linked into it, never by a list kept here — such a list would be
//! wrong the moment that library is updated. A malformed or unfamiliar header
//! is answered with `None` rather than an error.

use std::io::Read;
use std::path::Path;

/// How much of a file to read looking for its header. The tensor table comes
/// after the tokenizer's vocabulary, which runs to several megabytes for
/// large vocabularies, so the table is only reachable with room to spare; the
/// ceiling keeps a corrupt length field from asking for the world.
const HEADER_BUDGET: usize = 32 * 1024 * 1024;

const MAGIC: &[u8; 4] = b"GGUF";

/// The little a rejected file can still be asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufInfo {
    pub version: u32,
    pub tensor_count: u64,
    /// `general.architecture`: the model family llama.cpp is asked to build.
    pub architecture: Option<String>,
    /// Bytes of KV cache one token of context costs, when the header states
    /// the attention geometry. `None` when it does not.
    pub kv_bytes_per_token: Option<u64>,
    /// Distinct ggml type ids the file's tensors are stored in, ascending.
    ///
    /// `None` means the tensor table was not reached — the header ran past
    /// the bytes available — and says nothing about what is in it. An empty
    /// vector would mean a file with no tensors, which is a different thing,
    /// so the distinction is kept.
    pub tensor_types: Option<Vec<u32>>,
    /// `general.name`: the model's name as its publisher wrote it.
    pub name: Option<String>,
    /// `general.size_label`, such as "4B" or "8x7B".
    pub size_label: Option<String>,
    /// Weights in the file, summed over the tensor table; `None` when the
    /// table was not reached.
    pub parameter_count: Option<u64>,
}

/// Whether a build can be expected to load a file, judged from its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runnable {
    /// Every tensor is stored in a format this build implements.
    Yes,
    /// At least one tensor type is outside what this build knows. llama.cpp
    /// refuses these outright, so this is a certainty rather than a guess.
    No { unsupported: Vec<u32> },
    /// Not enough was read, or this build has no inference engine compiled
    /// in and so has nothing to compare against.
    Unknown,
}

impl GgufInfo {
    /// The model's size in billions of weights, rounded and at least 1:
    /// counted from the tensor table when it was read, else taken from
    /// `general.size_label`. A file name can say anything; the tensors are
    /// what the model is.
    pub fn params_b(&self) -> Option<u16> {
        let billions = match self.parameter_count {
            Some(n) if n > 0 => n as f64 / 1e9,
            _ => crate::model_catalog::parse_params_b(self.size_label.as_deref()?)? as f64,
        };
        Some(billions.round().clamp(1.0, u16::MAX as f64) as u16)
    }

    /// Judges the file against the tensor formats `type_count` covers, which
    /// is ggml's `GGML_TYPE_COUNT` for the build doing the asking.
    ///
    /// Only the storage format is checked. A file can pass here and still be
    /// refused for a model architecture the build does not implement; what
    /// this catches is the case that cannot be argued with, and the one that
    /// was sending people to download gigabytes of a format their engine has
    /// never heard of.
    pub fn runnable_with(&self, type_count: Option<u32>) -> Runnable {
        let (Some(limit), Some(types)) = (type_count, self.tensor_types.as_ref()) else {
            return Runnable::Unknown;
        };
        let unsupported: Vec<u32> = types.iter().copied().filter(|t| *t >= limit).collect();
        if unsupported.is_empty() {
            Runnable::Yes
        } else {
            Runnable::No { unsupported }
        }
    }
}

/// Reads the front of a GGUF file. `None` when the file is unreadable or is
/// not a GGUF at all — in which case there is nothing to add to the error.
pub fn probe(path: &Path) -> Option<GgufInfo> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEADER_BUDGET];
    let read = read_up_to(&mut file, &mut buf)?;
    buf.truncate(read);
    parse(&buf)
}

/// Reads a header out of bytes already in hand.
///
/// Separate from [`probe`] so the same parser serves a file on disk and the
/// first megabytes of one fetched over HTTP — deciding whether a download is
/// worth starting uses exactly the code that reads the finished file.
pub fn parse(bytes: &[u8]) -> Option<GgufInfo> {
    let mut r = Cursor::new(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let version = r.u32()?;
    let tensor_count = r.u64()?;
    let kv_count = r.u64()?;

    let (meta, geometry) = find_metadata(&mut r, kv_count);
    // `find_metadata` leaves the cursor after the key/value block only when
    // it walked all of it; a short read or an unmeasurable field stops it
    // early, and the tensor table cannot then be located.
    let table = if r.walked_all_kv {
        read_tensor_table(&mut r, tensor_count)
    } else {
        None
    };
    let (tensor_types, parameter_count) = match table {
        Some((types, params)) => (Some(types), params),
        None => (None, None),
    };

    Some(GgufInfo {
        version,
        tensor_count,
        architecture: meta.architecture,
        kv_bytes_per_token: geometry.kv_bytes_per_token(),
        tensor_types,
        name: meta.name,
        size_label: meta.size_label,
        parameter_count,
    })
}

/// The attention shape a model's KV cache is sized from.
///
/// Every key is prefixed with the architecture's own name ("qwen3.block_count"),
/// so the suffix is what is matched: the prefix is only known once the
/// architecture has been read, which may happen after these.
#[derive(Default)]
struct Geometry {
    block_count: Option<u64>,
    head_count_kv: Option<u64>,
    key_length: Option<u64>,
    value_length: Option<u64>,
}

impl Geometry {
    fn note(&mut self, key: &str, value: u64) {
        let field = match key.rsplit('.').next().unwrap_or(key) {
            "block_count" => &mut self.block_count,
            "head_count_kv" => &mut self.head_count_kv,
            "key_length" => &mut self.key_length,
            "value_length" => &mut self.value_length,
            _ => return,
        };
        *field = Some(value);
    }

    /// Bytes of KV cache per token of context.
    ///
    /// Each layer stores one key and one value vector per attention head that
    /// has its own — the grouped-query count, not the total, which is what
    /// makes this far smaller on modern models than head count alone
    /// suggests. llama.cpp's cache is f16 unless asked otherwise, hence the
    /// two bytes per element.
    fn kv_bytes_per_token(&self) -> Option<u64> {
        const BYTES_PER_ELEMENT: u64 = 2;
        let layers = self.block_count?;
        let kv_heads = self.head_count_kv?;
        let k = self.key_length?;
        let v = self.value_length?;
        layers
            .checked_mul(kv_heads)?
            .checked_mul(k.checked_add(v)?)?
            .checked_mul(BYTES_PER_ELEMENT)
    }
}

/// The distinct type ids of every tensor the header declares, and the
/// number of weights they hold (`None` if a size overflows).
///
/// All of them: one unsupported tensor is enough for llama.cpp to refuse the
/// file, so a partial answer would be worse than none.
fn read_tensor_table(r: &mut Cursor<'_>, tensor_count: u64) -> Option<(Vec<u32>, Option<u64>)> {
    let mut types: Vec<u32> = Vec::new();
    let mut params: Option<u64> = Some(0);
    for _ in 0..tensor_count {
        r.string()?; // name
        let dims = r.u32()?;
        let mut elements: Option<u64> = Some(1);
        for _ in 0..dims {
            let d = r.u64()?;
            elements = elements.and_then(|e| e.checked_mul(d));
        }
        params = params.zip(elements).and_then(|(p, e)| p.checked_add(e));
        let ty = r.u32()?;
        r.take(8)?; // offset
        if !types.contains(&ty) {
            types.push(ty);
        }
    }
    types.sort_unstable();
    Some((types, params))
}

/// The `general.*` strings this reader keeps.
#[derive(Default)]
struct Metadata {
    architecture: Option<String>,
    name: Option<String>,
    size_label: Option<String>,
}

/// Walks key/value pairs keeping `general.architecture`, `general.name` and
/// `general.size_label`. Stops at the first value it cannot measure:
/// skipping a field of unknown width would put the cursor somewhere
/// arbitrary, and a guess read out of the middle of a tensor is worse than
/// no answer.
fn find_metadata(r: &mut Cursor<'_>, kv_count: u64) -> (Metadata, Geometry) {
    let mut meta = Metadata::default();
    let mut geometry = Geometry::default();
    for _ in 0..kv_count {
        let Some(key) = r.string() else {
            return (meta, geometry);
        };
        let Some(value_type) = r.u32() else {
            return (meta, geometry);
        };
        if let Some(n) = r.peek_unsigned(value_type) {
            geometry.note(&key, n);
        }
        let slot = match key.as_str() {
            "general.architecture" => Some(&mut meta.architecture),
            "general.name" => Some(&mut meta.name),
            "general.size_label" => Some(&mut meta.size_label),
            _ => None,
        };
        if let (Some(slot), TYPE_STRING) = (slot, value_type) {
            // A value that cannot be read means the header ran out inside it,
            // and the cursor is then lost: nothing after this point could be
            // located, so the walk ends rather than reporting a position it
            // no longer knows.
            let Some(value) = r.string() else {
                *slot = None;
                return (meta, geometry);
            };
            *slot = Some(value);
            continue;
        }
        if r.skip_value(value_type).is_none() {
            return (meta, geometry);
        }
    }
    r.walked_all_kv = true;
    (meta, geometry)
}

const TYPE_STRING: u32 = 8;
const TYPE_ARRAY: u32 = 9;

/// Width of each fixed-size GGUF value type, indexed by its tag.
fn scalar_width(value_type: u32) -> Option<usize> {
    Some(match value_type {
        0 | 1 | 7 => 1, // u8, i8, bool
        2 | 3 => 2,     // u16, i16
        4..=6 => 4,     // u32, i32, f32
        10..=12 => 8,   // u64, i64, f64
        _ => return None,
    })
}

/// A bounds-checked reader over the bytes actually read.
///
/// Every length in a GGUF header comes from the file, so each one is treated
/// as a claim rather than a fact: a read that would run past the buffer
/// returns `None` instead of panicking.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Set once every key/value pair has been stepped over, which is the only
    /// state in which the cursor is known to sit on the tensor table.
    walked_all_kv: bool,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            walked_all_kv: false,
        }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = usize::try_from(self.u64()?).ok()?;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    /// Reads an unsigned scalar without moving, for the few numeric fields
    /// worth keeping while walking past everything else.
    fn peek_unsigned(&self, value_type: u32) -> Option<u64> {
        let width = match value_type {
            0 => 1usize, // u8
            2 => 2,      // u16
            4 => 4,      // u32
            10 => 8,     // u64
            _ => return None,
        };
        let end = self.pos.checked_add(width)?;
        let bytes = self.bytes.get(self.pos..end)?;
        let mut buf = [0u8; 8];
        buf[..width].copy_from_slice(bytes);
        Some(u64::from_le_bytes(buf))
    }

    /// Advances past one value without interpreting it.
    fn skip_value(&mut self, value_type: u32) -> Option<()> {
        match value_type {
            TYPE_STRING => {
                let len = usize::try_from(self.u64()?).ok()?;
                self.take(len)?;
            }
            TYPE_ARRAY => {
                let element_type = self.u32()?;
                let len = usize::try_from(self.u64()?).ok()?;
                if element_type == TYPE_STRING {
                    for _ in 0..len {
                        let n = usize::try_from(self.u64()?).ok()?;
                        self.take(n)?;
                    }
                } else {
                    self.take(scalar_width(element_type)?.checked_mul(len)?)?;
                }
            }
            other => {
                self.take(scalar_width(other)?)?;
            }
        }
        Some(())
    }
}

/// Fills as much of `buf` as the file has. A header shorter than the budget is
/// normal for a small file and is not an error.
fn read_up_to(file: &mut std::fs::File, buf: &mut [u8]) -> Option<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a header with the given key/value pairs, as GGUF lays them out.
    struct HeaderBuilder {
        body: Vec<u8>,
        kv_count: u64,
        /// Dimensions of each tensor; `None` writes no table.
        tensors: Option<Vec<Vec<u64>>>,
    }

    impl HeaderBuilder {
        fn new() -> Self {
            Self {
                body: Vec::new(),
                kv_count: 0,
                tensors: None,
            }
        }

        fn push_str(body: &mut Vec<u8>, s: &str) {
            body.extend_from_slice(&(s.len() as u64).to_le_bytes());
            body.extend_from_slice(s.as_bytes());
        }

        fn string_kv(mut self, key: &str, value: &str) -> Self {
            Self::push_str(&mut self.body, key);
            self.body.extend_from_slice(&TYPE_STRING.to_le_bytes());
            Self::push_str(&mut self.body, value);
            self.kv_count += 1;
            self
        }

        fn u32_kv(mut self, key: &str, value: u32) -> Self {
            Self::push_str(&mut self.body, key);
            self.body.extend_from_slice(&4u32.to_le_bytes());
            self.body.extend_from_slice(&value.to_le_bytes());
            self.kv_count += 1;
            self
        }

        fn str_array_kv(mut self, key: &str, values: &[&str]) -> Self {
            Self::push_str(&mut self.body, key);
            self.body.extend_from_slice(&TYPE_ARRAY.to_le_bytes());
            self.body.extend_from_slice(&TYPE_STRING.to_le_bytes());
            self.body
                .extend_from_slice(&(values.len() as u64).to_le_bytes());
            for v in values {
                Self::push_str(&mut self.body, v);
            }
            self.kv_count += 1;
            self
        }

        fn tensors(mut self, dims: &[&[u64]]) -> Self {
            self.tensors = Some(dims.iter().map(|d| d.to_vec()).collect());
            self
        }

        fn build(self) -> Vec<u8> {
            let mut out = Vec::from(*MAGIC);
            out.extend_from_slice(&3u32.to_le_bytes());
            let count = self.tensors.as_ref().map_or(851, |t| t.len() as u64);
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&self.kv_count.to_le_bytes());
            out.extend_from_slice(&self.body);
            for (i, dims) in self.tensors.iter().flatten().enumerate() {
                Self::push_str(&mut out, &format!("blk.{i}.weight"));
                out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
                for d in dims {
                    out.extend_from_slice(&d.to_le_bytes());
                }
                out.extend_from_slice(&12u32.to_le_bytes()); // Q4_K
                out.extend_from_slice(&0u64.to_le_bytes()); // offset
            }
            out
        }
    }

    fn probe_bytes(bytes: &[u8]) -> Option<GgufInfo> {
        let path = std::env::temp_dir().join(format!(
            "nepheral-gguf-probe-{}-{:?}.gguf",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, bytes).expect("write fixture");
        let info = probe(&path);
        let _ = std::fs::remove_file(&path);
        info
    }

    #[test]
    fn it_reads_the_architecture_a_file_declares() {
        let bytes = HeaderBuilder::new()
            .string_kv("general.architecture", "qwen35")
            .build();
        let info = probe_bytes(&bytes).expect("a well-formed header is readable");
        assert_eq!(info.architecture.as_deref(), Some("qwen35"));
        assert_eq!(info.version, 3);
        assert_eq!(info.tensor_count, 851);
    }

    #[test]
    fn it_steps_over_earlier_keys_to_reach_the_architecture() {
        // Real files put the tokenizer's vocabulary in front of much of the
        // metadata, so skipping has to be exact or the answer lands in the
        // middle of another field.
        let bytes = HeaderBuilder::new()
            .u32_kv("general.quantization_version", 2)
            .str_array_kv("tokenizer.ggml.tokens", &["<s>", "</s>", "hola"])
            .string_kv("general.architecture", "llama")
            .build();
        assert_eq!(
            probe_bytes(&bytes).and_then(|i| i.architecture).as_deref(),
            Some("llama")
        );
    }

    #[test]
    fn an_unmeasurable_field_stops_the_walk_instead_of_guessing() {
        // A tag this reader does not know has unknown width, so the cursor
        // cannot be advanced past it. Reporting nothing beats reporting a
        // string read from an arbitrary offset.
        let mut body = Vec::new();
        HeaderBuilder::push_str(&mut body, "an.unknown.field");
        body.extend_from_slice(&21u32.to_le_bytes());
        body.extend_from_slice(&[0xAB; 16]);
        HeaderBuilder::push_str(&mut body, "general.architecture");
        body.extend_from_slice(&TYPE_STRING.to_le_bytes());
        HeaderBuilder::push_str(&mut body, "llama");

        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&851u64.to_le_bytes());
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&body);

        let info = probe_bytes(&bytes).expect("the fixed header is still readable");
        assert_eq!(
            info.architecture, None,
            "the architecture sits behind a field of unknown width and must not be guessed at"
        );
    }

    #[test]
    fn a_file_that_is_not_a_gguf_is_not_diagnosed() {
        assert!(probe_bytes(b"not a model at all").is_none());
    }

    #[test]
    fn a_truncated_header_does_not_panic() {
        let full = HeaderBuilder::new()
            .string_kv("general.architecture", "qwen3")
            .build();
        for cut in 0..full.len() {
            let _ = probe_bytes(&full[..cut]);
        }
    }

    #[test]
    fn a_lying_length_does_not_panic() {
        // Every length in the header is attacker-controlled once a model is
        // downloaded from anywhere, so an absurd one has to be refused rather
        // than allocated or indexed.
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        bytes.extend_from_slice(b"general.architecture");
        assert!(probe_bytes(&bytes).is_some_and(|i| i.architecture.is_none()));
    }

    fn info(types: Option<Vec<u32>>) -> GgufInfo {
        GgufInfo {
            version: 3,
            tensor_count: 4,
            architecture: Some("qwen3".to_string()),
            kv_bytes_per_token: None,
            tensor_types: types,
            name: None,
            size_label: None,
            parameter_count: None,
        }
    }

    #[test]
    fn it_reads_the_name_and_counts_the_weights() {
        // A file named "model.gguf" still says what it is inside.
        let bytes = HeaderBuilder::new()
            .string_kv("general.architecture", "qwen3")
            .string_kv("general.name", "Qwen3 4B Instruct 2507")
            .string_kv("general.size_label", "4B")
            .tensors(&[&[2560, 151_936], &[2560, 9728, 36], &[2560, 2560, 36, 4]])
            .build();
        let info = probe_bytes(&bytes).expect("readable");
        assert_eq!(info.name.as_deref(), Some("Qwen3 4B Instruct 2507"));
        assert_eq!(info.size_label.as_deref(), Some("4B"));
        let expected = 2560 * 151_936 + 2560 * 9728 * 36 + 2560 * 2560 * 36 * 4;
        assert_eq!(info.parameter_count, Some(expected));
        assert_eq!(
            info.params_b(),
            Some(2),
            "counted weights win over the label"
        );
    }

    #[test]
    fn without_a_table_the_size_label_is_used() {
        let bytes = HeaderBuilder::new()
            .string_kv("general.size_label", "14B")
            .build();
        let info = probe_bytes(&bytes).expect("readable");
        assert_eq!(info.parameter_count, None);
        assert_eq!(info.params_b(), Some(14));
        let small = HeaderBuilder::new()
            .string_kv("general.size_label", "0.6B")
            .build();
        assert_eq!(probe_bytes(&small).and_then(|i| i.params_b()), Some(1));
    }

    #[test]
    fn a_file_using_only_known_formats_is_runnable() {
        assert_eq!(
            info(Some(vec![0, 1, 12])).runnable_with(Some(43)),
            Runnable::Yes
        );
    }

    #[test]
    fn a_format_outside_the_build_is_refused_with_its_id() {
        // The case this exists for: a fork's ternary packing carries a type id
        // far outside ggml's range, and llama.cpp refuses it outright. The id
        // is reported so the verdict can be checked rather than believed.
        assert_eq!(
            info(Some(vec![0, 142])).runnable_with(Some(43)),
            Runnable::No {
                unsupported: vec![142]
            }
        );
        assert_eq!(
            info(Some(vec![0, 142, 143])).runnable_with(Some(43)),
            Runnable::No {
                unsupported: vec![142, 143]
            }
        );
    }

    #[test]
    fn the_boundary_id_is_out_of_range() {
        // GGML_TYPE_COUNT is one past the last valid id, so a tensor stored as
        // the count itself is exactly the off-by-one worth pinning.
        assert_eq!(info(Some(vec![42])).runnable_with(Some(43)), Runnable::Yes);
        assert_eq!(
            info(Some(vec![43])).runnable_with(Some(43)),
            Runnable::No {
                unsupported: vec![43]
            }
        );
    }

    #[test]
    fn an_unread_tensor_table_is_not_a_pass() {
        // Saying "runs here" because the header was too short to check is the
        // failure this whole path removes.
        assert_eq!(info(None).runnable_with(Some(43)), Runnable::Unknown);
    }

    #[test]
    fn a_build_with_no_engine_judges_nothing() {
        assert_eq!(
            info(Some(vec![0, 142])).runnable_with(None),
            Runnable::Unknown
        );
    }

    #[test]
    fn tensor_types_are_read_from_the_table() {
        let mut body = Vec::new();
        HeaderBuilder::push_str(&mut body, "general.architecture");
        body.extend_from_slice(&TYPE_STRING.to_le_bytes());
        HeaderBuilder::push_str(&mut body, "qwen35");

        let mut tensors = Vec::new();
        for (name, ty) in [("token_embd.weight", 0u32), ("output.weight", 142u32)] {
            HeaderBuilder::push_str(&mut tensors, name);
            tensors.extend_from_slice(&1u32.to_le_bytes());
            tensors.extend_from_slice(&8u64.to_le_bytes());
            tensors.extend_from_slice(&ty.to_le_bytes());
            tensors.extend_from_slice(&0u64.to_le_bytes());
        }

        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(&tensors);

        let info = parse(&bytes).expect("readable");
        assert_eq!(info.architecture.as_deref(), Some("qwen35"));
        assert_eq!(info.tensor_types, Some(vec![0, 142]));
        assert_eq!(
            info.runnable_with(Some(43)),
            Runnable::No {
                unsupported: vec![142]
            }
        );
    }

    #[test]
    fn a_header_cut_before_the_tensor_table_reports_nothing_about_it() {
        // The builder writes no tensor table, so the declared count cannot be
        // satisfied: the types must come back unknown, not empty.
        let bytes = HeaderBuilder::new()
            .string_kv("general.architecture", "llama")
            .build();
        let info = parse(&bytes).expect("readable");
        assert_eq!(info.tensor_types, None);
    }
}
