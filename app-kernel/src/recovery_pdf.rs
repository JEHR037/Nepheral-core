// SPDX-License-Identifier: BUSL-1.1
//! The recovery phrase as a password-protected PDF.
//!
//! Paper is the recommended backup and the document says so. This is the
//! extra copy for people who want one: a single A4 page whose content is
//! encrypted with the PDF standard security handler, revision 6 (AES-256,
//! ISO 32000-2 §7.6.4), so any current reader opens it with the password and
//! nothing without it. The owner password is random and thrown away, so the
//! permissions (print, no copying) cannot be lifted.
//!
//! Written by hand rather than through a PDF library: the page is a few
//! lines of text, and the encryption is the part that has to be right.

use aes::cipher::{block_padding, BlockEncrypt, BlockEncryptMut, KeyInit, KeyIvInit};
use rand::RngCore;
use sha2::{Digest, Sha256, Sha384, Sha512};
use zeroize::Zeroizing;

/// The shortest password the export accepts.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Print allowed, copying and editing not. Bits 7-8 and 13-32 are reserved
/// and must be set (ISO 32000-2, table 22).
const PERMISSIONS: i32 = -1852;

struct Labels {
    title: &'static str,
    lead: &'static str,
    wallet: &'static str,
    paper: &'static str,
    lines: &'static [&'static str],
}

const EN: Labels = Labels {
    title: "Nepheral \u{2014} recovery phrase",
    lead: "These 24 words are your wallet. Whoever has them controls it and its funds.",
    wallet: "Wallet",
    paper: "The recommended backup is a handwritten copy on paper, kept somewhere safe.",
    lines: &[
        "This PDF is an extra copy. Keep it off the computer that runs Nepheral (a USB drive",
        "you store away, for example). Do not email it or upload it to the cloud. If you",
        "print it, delete the file afterwards.",
        "",
        "Nepheral will never ask you for these words. To recover the wallet, open Nepheral,",
        "choose \u{201c}Restore from your phrase\u{201d} and type the words in this order.",
    ],
};

const ES: Labels = Labels {
    title: "Nepheral \u{2014} frase de recuperaci\u{f3}n",
    lead: "Estas 24 palabras son tu cartera. Quien las tenga la controla, con sus fondos.",
    wallet: "Cartera",
    paper: "Lo m\u{e1}s recomendado es copiarlas a mano en papel y guardarlo en un lugar seguro.",
    lines: &[
        "Este PDF es una copia adicional. Gu\u{e1}rdalo fuera del equipo donde corre Nepheral",
        "(por ejemplo, en una memoria USB que guardes aparte). No lo env\u{ed}es por correo ni",
        "lo subas a la nube. Si lo imprimes, borra el archivo despu\u{e9}s.",
        "",
        "Nepheral nunca te pedir\u{e1} estas palabras. Para recuperar la cartera, abre Nepheral,",
        "elige \u{ab}Restaurar con tu frase\u{bb} y escribe las palabras en este orden.",
    ],
};

/// Builds the encrypted PDF. `address` is the wallet's public address, shown
/// so the copy can be matched to its wallet; `lang` is "es" or anything else
/// for English.
pub fn build(words: &[&str], address: &str, lang: &str, password: &str) -> Result<Vec<u8>, String> {
    if words.len() != 24 {
        return Err("a recovery phrase has 24 words".into());
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!(
            "the password must have at least {MIN_PASSWORD_CHARS} characters"
        ));
    }
    let labels = if lang == "es" { &ES } else { &EN };
    let content = page_content(words, address, labels);

    let mut rng = rand::rngs::OsRng;
    let mut file_key = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut file_key[..]);
    let mut owner = Zeroizing::new([0u8; 32]);
    rng.fill_bytes(&mut owner[..]);
    let user_pw = password_bytes(password);
    let owner_pw = Zeroizing::new(hex::encode(&owner[..]).into_bytes());
    let sec = security(&file_key, &user_pw, &owner_pw, &mut rng);

    let mut id = [0u8; 16];
    rng.fill_bytes(&mut id);
    let stream = encrypt_aesv3(&file_key, &content, &mut rng);
    Ok(assemble(&stream, &sec, &id))
}

/// SASLprep is left out: it only changes passwords with characters that have
/// compatibility forms, and the export's own field will not produce those.
/// Revision 6 reads at most 127 bytes of the password.
fn password_bytes(password: &str) -> Zeroizing<Vec<u8>> {
    let bytes = password.as_bytes();
    Zeroizing::new(bytes[..bytes.len().min(127)].to_vec())
}

struct Security {
    u: [u8; 48],
    ue: [u8; 32],
    o: [u8; 48],
    oe: [u8; 32],
    perms: [u8; 16],
}

/// Algorithms 8, 9 and 10 of ISO 32000-2.
fn security(
    file_key: &[u8; 32],
    user_pw: &[u8],
    owner_pw: &[u8],
    rng: &mut impl RngCore,
) -> Security {
    let mut salts = [0u8; 32];
    rng.fill_bytes(&mut salts);
    let (u_valid, rest) = salts.split_at(8);
    let (u_key, rest) = rest.split_at(8);
    let (o_valid, o_key) = rest.split_at(8);

    let mut u = [0u8; 48];
    u[..32].copy_from_slice(&hash_r6(user_pw, u_valid, &[])[..]);
    u[32..40].copy_from_slice(u_valid);
    u[40..].copy_from_slice(u_key);
    let ue = wrap_key(&hash_r6(user_pw, u_key, &[]), file_key);

    let mut o = [0u8; 48];
    o[..32].copy_from_slice(&hash_r6(owner_pw, o_valid, &u)[..]);
    o[32..40].copy_from_slice(o_valid);
    o[40..].copy_from_slice(o_key);
    let oe = wrap_key(&hash_r6(owner_pw, o_key, &u), file_key);

    let mut block = [0u8; 16];
    block[..4].copy_from_slice(&PERMISSIONS.to_le_bytes());
    block[4..8].copy_from_slice(&[0xff; 4]);
    block[8..12].copy_from_slice(b"Tadb");
    rng.fill_bytes(&mut block[12..]);
    let cipher = aes::Aes256::new(file_key.into());
    let mut b = block.into();
    cipher.encrypt_block(&mut b);
    let mut perms = [0u8; 16];
    perms.copy_from_slice(&b);

    Security {
        u,
        ue,
        o,
        oe,
        perms,
    }
}

/// The file key under a password-derived key: AES-256-CBC, zero IV, no
/// padding, as revision 6 specifies for /UE and /OE.
fn wrap_key(key: &[u8; 32], file_key: &[u8; 32]) -> [u8; 32] {
    let enc = cbc::Encryptor::<aes::Aes256>::new(key.into(), &[0u8; 16].into());
    let out = enc.encrypt_padded_vec_mut::<block_padding::NoPadding>(file_key);
    let mut wrapped = [0u8; 32];
    wrapped.copy_from_slice(&out);
    wrapped
}

/// Algorithm 2.B: the iterated hash of revision 6.
fn hash_r6(password: &[u8], salt: &[u8], udata: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut k: Zeroizing<Vec<u8>> = Zeroizing::new(
        Sha256::new()
            .chain_update(password)
            .chain_update(salt)
            .chain_update(udata)
            .finalize()
            .to_vec(),
    );
    let mut round = 0usize;
    loop {
        let mut unit = Zeroizing::new(Vec::with_capacity(password.len() + k.len() + udata.len()));
        unit.extend_from_slice(password);
        unit.extend_from_slice(&k);
        unit.extend_from_slice(udata);
        let k1 = Zeroizing::new(unit.repeat(64));
        let enc = cbc::Encryptor::<aes::Aes128>::new(k[..16].into(), k[16..32].into());
        let e = Zeroizing::new(enc.encrypt_padded_vec_mut::<block_padding::NoPadding>(&k1));
        // The first 16 bytes as a big-endian number, mod 3; 256 = 1 (mod 3).
        let pick = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = Zeroizing::new(match pick {
            0 => Sha256::digest(&e[..]).to_vec(),
            1 => Sha384::digest(&e[..]).to_vec(),
            _ => Sha512::digest(&e[..]).to_vec(),
        });
        round += 1;
        let last = *e.last().expect("non-empty") as usize;
        if round >= 64 && last + 32 <= round {
            break;
        }
    }
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&k[..32]);
    out
}

/// A string or stream under AESV3: a random IV, then AES-256-CBC with PKCS#7
/// padding, keyed by the file key itself.
fn encrypt_aesv3(file_key: &[u8; 32], data: &[u8], rng: &mut impl RngCore) -> Vec<u8> {
    let mut iv = [0u8; 16];
    rng.fill_bytes(&mut iv);
    let enc = cbc::Encryptor::<aes::Aes256>::new(file_key.into(), &iv.into());
    let mut out = iv.to_vec();
    out.extend(enc.encrypt_padded_vec_mut::<block_padding::Pkcs7>(data));
    out
}

/// Text as a PDF literal string in WinAnsiEncoding. Characters outside it
/// become "?"; the labels and BIP-39 words stay inside it.
fn lit(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for c in s.chars() {
        let b = match c {
            '(' | ')' | '\\' => {
                out.push(b'\\');
                c as u8
            }
            ' '..='~' => c as u8,
            '\u{a0}'..='\u{ff}' => c as u32 as u8,
            '\u{2014}' => 0x97,
            '\u{2013}' => 0x96,
            '\u{2018}' => 0x91,
            '\u{2019}' => 0x92,
            '\u{201c}' => 0x93,
            '\u{201d}' => 0x94,
            '\u{2022}' => 0x95,
            '\u{2026}' => 0x85,
            _ => b'?',
        };
        out.push(b);
    }
    out.push(b')');
    out
}

fn text(out: &mut Vec<u8>, font: &str, size: f32, x: f32, y: f32, s: &str) {
    out.extend(format!("BT /{font} {size} Tf {x} {y} Td ").bytes());
    out.extend(lit(s));
    out.extend(b" Tj ET\n");
}

fn page_content(words: &[&str], address: &str, l: &Labels) -> Zeroizing<Vec<u8>> {
    let mut c = Zeroizing::new(Vec::with_capacity(4096));
    // Accent rule and title.
    c.extend(b"0.40 0.78 0.85 RG 2 w 56 790 m 116 790 l S\n0.08 0.09 0.11 rg\n");
    text(&mut c, "F2", 20.0, 56.0, 760.0, l.title);
    c.extend(b"0.30 0.32 0.36 rg\n");
    text(&mut c, "F1", 11.0, 56.0, 738.0, l.lead);

    // The words: three columns of eight, numbered, each in a light box.
    let (col_w, row_h, top) = (161.0f32, 30.0f32, 700.0f32);
    c.extend(b"0.78 0.80 0.84 RG 0.6 w\n");
    for i in 0..24 {
        let (col, row) = ((i / 8) as f32, (i % 8) as f32);
        let x = 56.0 + col * col_w;
        let y = top - row * row_h;
        c.extend(format!("{} {} {} {} re S\n", x, y - 9.0, col_w - 12.0, 24.0).bytes());
    }
    for (i, w) in words.iter().enumerate() {
        let (col, row) = ((i / 8) as f32, (i % 8) as f32);
        let x = 56.0 + col * col_w;
        let y = top - row * row_h;
        c.extend(b"0.55 0.57 0.62 rg\n");
        text(&mut c, "F1", 9.0, x + 8.0, y, &format!("{:>2}", i + 1));
        c.extend(b"0.08 0.09 0.11 rg\n");
        text(&mut c, "F3", 13.0, x + 26.0, y, w);
    }

    let mut y = top - 8.0 * row_h - 10.0;
    c.extend(b"0.30 0.32 0.36 rg\n");
    text(
        &mut c,
        "F1",
        9.0,
        56.0,
        y,
        &format!("{}: {address}", l.wallet),
    );
    y -= 30.0;
    // The recommendation stands out: amber bar beside it, bold.
    c.extend(format!("0.95 0.70 0.25 rg 56 {} 3 18 re f\n", y - 5.0).bytes());
    c.extend(b"0.08 0.09 0.11 rg\n");
    text(&mut c, "F2", 11.5, 66.0, y, l.paper);
    y -= 28.0;
    c.extend(b"0.25 0.27 0.31 rg\n");
    for line in l.lines {
        if !line.is_empty() {
            text(&mut c, "F1", 10.5, 56.0, y, line);
        }
        y -= 15.0;
    }
    c
}

/// The file: catalog, page tree, page, content, three standard fonts and the
/// encryption dictionary, then the cross-reference table and trailer.
fn assemble(stream: &[u8], sec: &Security, id: &[u8; 16]) -> Vec<u8> {
    let font = |base: &str| {
        format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>")
            .into_bytes()
    };
    let mut content = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
    content.extend_from_slice(stream);
    content.extend(b"\nendstream");
    let encrypt = format!(
        "<< /Filter /Standard /V 5 /R 6 /Length 256 \
         /CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV3 /Length 32 >> >> \
         /StmF /StdCF /StrF /StdCF /P {PERMISSIONS} /EncryptMetadata true \
         /U <{}> /UE <{}> /O <{}> /OE <{}> /Perms <{}> >>",
        hex::encode(sec.u),
        hex::encode(sec.ue),
        hex::encode(sec.o),
        hex::encode(sec.oe),
        hex::encode(sec.perms),
    );
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R \
          /Extensions << /ADBE << /BaseVersion /1.7 /ExtensionLevel 8 >> >> >>"
            .to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
          /Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 7 0 R >> >> /Contents 4 0 R >>"
            .to_vec(),
        content,
        font("Helvetica"),
        font("Helvetica-Bold"),
        font("Courier-Bold"),
        encrypt.into_bytes(),
    ];

    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend_from_slice(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objects.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n\r\n").bytes());
    }
    let id = hex::encode(id);
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Encrypt {} 0 R /ID [<{id}> <{id}>] >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1,
            objects.len(),
        )
        .bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockDecryptMut, KeyIvInit};

    const WORDS: &str = "abandon ability able about above absent absorb abstract absurd abuse \
        access accident account accuse achieve acid acoustic acquire across act action actor \
        actress actual";

    fn words() -> Vec<&'static str> {
        WORDS.split_whitespace().collect()
    }

    fn hex_field(pdf: &[u8], name: &str) -> Vec<u8> {
        let s = String::from_utf8_lossy(pdf);
        let at = s.find(&format!("/{name} <")).expect(name) + name.len() + 3;
        let end = at + s[at..].find('>').unwrap();
        hex::decode(&s[at..end]).unwrap()
    }

    // Vectors from pypdf's `AlgV5.calculate_hash(6, ...)`, an independent
    // implementation of the same algorithm.
    #[test]
    fn the_revision_6_hash_matches_another_implementation() {
        let salt: Vec<u8> = (0..8).collect();
        assert_eq!(
            hex::encode(&hash_r6(b"correct horse battery", &salt, &[])[..]),
            "fe4983d3a4beb3a69cc2790604f39d08e9dc3bbf360ca2a868b7e2b2ce1428fc"
        );
        let udata: Vec<u8> = (0..48).collect();
        assert_eq!(
            hex::encode(&hash_r6(b"correct horse battery", &salt, &udata)[..]),
            "745774d9eb841d26330fe141fb1829c23c228f4002be3f8c9be925898a24476c"
        );
    }

    #[test]
    fn no_word_is_readable_without_the_password() {
        let pdf = build(&words(), "ab12", "en", "correct horse battery").unwrap();
        assert!(pdf.starts_with(b"%PDF-1.7"));
        let text = String::from_utf8_lossy(&pdf);
        for w in words() {
            assert!(!text.contains(w), "{w} in the clear");
        }
        assert!(text.contains("/R 6"));
    }

    // Opens the file the way a reader does: check the password against /U,
    // unwrap the file key from /UE, decrypt the page.
    #[test]
    fn the_password_opens_the_page_and_a_wrong_one_does_not() {
        let pw = "correct horse battery";
        let pdf = build(&words(), "ab12", "es", pw).unwrap();
        let u = hex_field(&pdf, "U");
        let ue = hex_field(&pdf, "UE");
        assert_eq!(&hash_r6(pw.as_bytes(), &u[32..40], &[])[..], &u[..32]);
        assert_ne!(&hash_r6(b"wrong password", &u[32..40], &[])[..], &u[..32]);

        let kek = hash_r6(pw.as_bytes(), &u[40..48], &[]);
        let mut key = ue.clone();
        cbc::Decryptor::<aes::Aes256>::new(kek[..].into(), &[0u8; 16].into())
            .decrypt_padded_mut::<block_padding::NoPadding>(&mut key)
            .unwrap();

        let s = String::from_utf8_lossy(&pdf).into_owned();
        let len: usize = {
            let at = s.find("<< /Length ").unwrap() + 11;
            s[at..].split_whitespace().next().unwrap().parse().unwrap()
        };
        let start = pdf.windows(7).position(|w| w == b"stream\n").unwrap() + 7;
        let mut body = pdf[start + 16..start + len].to_vec();
        let plain =
            cbc::Decryptor::<aes::Aes256>::new(key[..].into(), pdf[start..start + 16].into())
                .decrypt_padded_mut::<block_padding::Pkcs7>(&mut body)
                .unwrap()
                .to_vec();
        let page = String::from_utf8_lossy(&plain);
        for w in words() {
            assert!(page.contains(&format!("({w})")), "{w} missing");
        }
        assert!(page.contains("papel"));
    }

    #[test]
    fn the_cross_reference_table_points_at_each_object() {
        let pdf = build(&words(), "ab12", "en", "correct horse battery").unwrap();
        // Offsets are in bytes and the stream is binary: search the bytes.
        let xref = pdf.windows(6).rposition(|w| w == b"\nxref\n").unwrap() + 1;
        let tail = std::str::from_utf8(&pdf[xref..]).unwrap();
        for (n, line) in tail.lines().skip(3).take(8).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            let obj = format!("{} 0 obj", n + 1);
            assert!(pdf[off..].starts_with(obj.as_bytes()), "object {}", n + 1);
        }
        let start: usize = tail
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(start, xref);
    }

    #[test]
    fn short_passwords_and_short_phrases_are_refused() {
        assert!(build(&words(), "ab12", "en", "short").is_err());
        assert!(build(&words()[..12], "ab12", "en", "correct horse battery").is_err());
    }
}
