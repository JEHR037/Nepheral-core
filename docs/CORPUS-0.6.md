# 0.6 corpus: openly licensed books for RAG

The books that 0.6's retrieval (RAG) may index, recommend
and serve from the mesh, from programming to medicine, through cooking and
civil engineering. Every licence was checked on 2026-10-04 on the
publisher's own page, the book's copyright page, or its repository's
LICENSE file; an item whose licence could not be confirmed is left out.

## Licence rules applied

- **Accepted:** public domain (PD), CC0, CC BY, CC BY-SA, GFDL, MIT/Apache/BSD-style software-doc licences (including PSF-2.0 and the PostgreSQL Licence), and US federal works under 17 U.S.C. §105.
- **Rejected:** anything NonCommercial (NC) or NoDerivatives (ND), "free to read" with no licence, and unclear terms.
- **Public domain in Spain:** authors who died before 7 Dec 1987 are protected for 80 years after death (TRLPI 4th transitional provision, which applies the 1879 law). Elsewhere in the EU it is 70 years. Every PD item below is PD in the US and Spain unless flagged.
- **Copyleft obligations:** for CC BY-SA and GFDL items, anything adapted from them (for example edited or translated chunks that the app redistributes) must carry the same licence. GFDL items must also ship the licence text and any "Invariant Sections".
- **Project Gutenberg:** strip the PG header, footer and every mention of PG. Gutenberg's licence says the text is then "unrestricted". If the PG name is kept, commercial use owes a 20% royalty and only verbatim copies are allowed (https://www.gutenberg.org/policy/license.html).
- **US federal works:** PD in the US, but foreign copyright may apply (NIST says so explicitly). Third-party figures inside them keep their own rights. "Except where otherwise noted" on Pressbooks titles means the same for some figures, so ingest the text and check image credits.
- **Metadata:** store the licence, attribution line, source URL and version or commit for every document in the index.

## Biggest finding: OpenStax switched to NC in March 2026

- **What changed:** OpenStax's own data (CMS API at `openstax.org/apps/cms/api/v2/pages/?type=books.Book`) and its help page (https://help.openstax.org/s/article/Licensing-information-of-OpenStax-textbooks) now list almost every current English edition as **CC BY-NC-SA 4.0**.
- **When:** the GitHub repos show commits titled "updating license to CC BY NC-SA" dated 2026-03-12 to 03-24.
- **Earlier versions stay CC BY 4.0.** The CC FAQ says: "CC licenses are not revocable… anyone who has access to a copy… may continue to redistribute it under the CC license terms" (https://creativecommons.org/faq/).
- **How to use them:** take the source at the last commit before the change. I confirmed `LICENSE` = "Attribution 4.0 International" and collection `md:license` = `by/4.0` at each commit below.
- **Caveats for the snapshots:**
  - You cannot pull in later errata, because those are NC-SA.
  - CC BY does not cover the OpenStax name or logo.
  - Some figures carry their own credits.
  - Since OpenStax now says "no commercial use", you could optionally also file their commercial-reuse request so they can't object later.

| Books (CC BY 4.0 at this commit) | Repo `github.com/openstax/...` | Last CC BY commit |
|---|---|---|
| Anatomy and Physiology 2e | osbooks-anatomy-physiology | `b0058a743bdd2cc08196abaaa7d5469970c0d80c` |
| Biology 2e, Concepts of Biology | osbooks-biology-bundle | `d2779c2edfb435e75047325434fe2fe93d5aa120` |
| University Physics Vol. 1–3 | osbooks-university-physics-bundle | `db44eb99e9aa1d918fd8cf182f82ccdece9c7b11` |
| College Physics 2e | osbooks-college-physics-bundle | `f98d7a792138a6133fe7267d17e70aa04e9ccbed` |
| Chemistry 2e, Chemistry: Atoms First 2e | osbooks-chemistry-bundle | `32c719622fbb7a0ca729cd645c1df4cf4e7d5b3a` |
| Astronomy 2e | osbooks-astronomy | `63157f370b09c06a4cc12832384aaaf4de1ced3b` |
| Microbiology | osbooks-microbiology | `ecf34dad129d686703780e87ab5aa2e59c577a6a` |
| Pharmacology for Nurses, Nutrition for Nurses, Population Health for Nurses | osbooks-nursing-external-bundle | `f0124920c65609137effe2333dea327d1c77accc` |
| Introductory Statistics 2e, Introductory Business Statistics 2e | osbooks-introductory-statistics-bundle | `50ec91749080bb040b57bb40c34337cacc994cba` |
| Algebra & Trigonometry 2e, College Algebra 2e, Precalculus 2e | osbooks-college-algebra-bundle | `4922e46ebc04326979e19392ccc2384cb8b9076c` |
| Introduction to Python Programming | osbooks-introduction-python-programming | `2825b114221917bea5ae328a94e484b4da8a93ca` |

**Still CC BY 4.0 today (pin these now):**
- **English:**
  - *Physics*: high-school level, copyright Texas Education Agency. Repo `osbooks-physics`.
  - *Statistics*: high-school level, copyright Texas Education Agency. Repo `osbooks-statistics`.
- **Spanish (repo LICENSE and collection metadata both say CC BY 4.0):**
  - *Física universitaria* vol. 1–3: `osbooks-fisica-universitaria-bundle` @ `d2e11f5c0c71`.
  - *Química 2ed* and *Química: Comenzando con los átomos 2ed*: `osbooks-quimica-bundle` @ `755d386f196f`.
  - *Introducción a la estadística* and *Introducción a la estadística empresarial*: `osbooks-introduccion-estadistica-bundle` @ `d240445975b5`.
  - *Precálculo 2ed*: `osbooks-precalculo` @ `c4d6f6c0e5ed`.
- **Excluded:** *Cálculo* vol. 1–3 is CC BY-NC-SA.

All OpenStax books come as HTML (web), PDF, and CNXML source in the repo.

---

## 1. Programming and computer science

| Title | Author / publisher | Licence | Lang | Formats | Verified at | Why it's useful |
|---|---|---|---|---|---|---|
| The Rust Programming Language | Klabnik, Nichols et al. / Rust Project | MIT OR Apache-2.0 | EN; ES (RustLangES, same licence) | HTML (mdBook), Markdown src | github.com/rust-lang/book/blob/main/COPYRIGHT ; github.com/RustLangES/rust-book-es (COPYRIGHT) | The standard Rust introduction. Use the repo, not the No Starch print edition. |
| Rust by Example; The Rust Reference; The Rustonomicon; The Cargo Book | Rust Project | MIT OR Apache-2.0 | EN | HTML, Markdown src | README of rust-lang/rust-by-example; LICENSE-MIT and LICENSE-APACHE in rust-lang/reference, /nomicon, /cargo | Worked examples, language spec, unsafe Rust, build tooling. |
| Comprehensive Rust | Google | Prose **CC BY 4.0**; code Apache-2.0 | EN, ES and others | HTML, Markdown src | github.com/google/comprehensive-rust README | Full course, including Android, concurrency and bare-metal. |
| The Embedded Rust Book | Rust Embedded WG | Prose **CC BY-SA 4.0**; code MIT/Apache | EN | HTML, Markdown src | github.com/rust-embedded/book README | Embedded and no_std Rust. |
| Python documentation (tutorial, library, language reference, HOWTOs) | Python Software Foundation | PSF License v2; code examples also 0BSD | EN; ES (python-docs-es: "same license as the Python package") | HTML, PDF, EPUB, TXT (docs.python.org/3/download.html), reST src | https://docs.python.org/3/license.html | Authoritative Python reference. PSF-2.0 is a permissive, OSI-approved BSD-style licence. |
| How to Think Like a Computer Scientist: Interactive Edition (Python 3) | Miller, Ranum, Elkner, Wentworth, Downey, Meyers, Mitchell / Runestone | **GFDL 1.3+** (Invariant Sections: Foreword, Prefaces, Contributor List) | EN | HTML, reST/PreTeXt src | github.com/RunestoneInteractive/thinkcspy (LICENSE.txt, `_sources/FrontBackMatter/copyright.rst`) | Python 3 for beginners. |
| Introduction to Python Programming | OpenStax | CC BY 4.0 **only at the snapshot commit** | EN | HTML, PDF, CNXML | see OpenStax table | College-level Python course text (2024). |
| Structure and Interpretation of Computer Programs, 2nd ed. | Abelson, Sussman, Sussman / MIT Press | **CC BY-SA 4.0** | EN | HTML | mitp-content-server.mit.edu/books/content/sectbyfn/books_pres_0/6515/sicp.zip/index.html (licence footer) | Classic foundations: abstraction, interpreters. |
| Algorithms (2019) | Jeff Erickson | **CC BY 4.0** (the textbook only) | EN | PDF | https://jeffe.cs.illinois.edu/teaching/algorithms/ | Rigorous algorithms text. |
| Open Data Structures | Pat Morin | **CC BY 2.5 Canada** (text and code) | EN | HTML, PDF, LaTeX, Java/C++/Python src | https://opendatastructures.org/ | Data structures with code; commercial use explicitly allowed. |
| MDN Web Docs | Mozilla contributors | Prose CC BY-SA 2.5 or later; code samples CC0 (MIT if added before 2010-08-20) | EN, ES and others | HTML, Markdown src (mdn/content, mdn/translated-content) | developer.mozilla.org/en-US/docs/MDN/Writing_guidelines/Attrib_copyright_license | Web platform: HTML, CSS, JS, HTTP. |
| SQLite docs; PostgreSQL docs | SQLite authors; PGDG | SQLite: **PD** dedication. PostgreSQL: PostgreSQL Licence (BSD/MIT-like) | EN | HTML (PDF for PG) | sqlite.org/copyright.html ; postgresql.org/about/licence/ | Databases and SQL. |
| Dive Into Python 3 | Mark Pilgrim | **CC BY-SA 3.0** (code MIT) | EN | HTML, repo | about.html in github.com/diveintomark/diveintopython3 | **Outdated** (Python 3.1 era, about 2009). Use as low-priority history only. |

## 2. Mathematics and statistics

| Title | Author / publisher | Licence | Lang | Formats | Verified at | Why it's useful |
|---|---|---|---|---|---|---|
| Mathematics for Computer Science (2018) | Lehman, Leighton, Meyer (MIT) | **CC BY-SA 3.0** (PDF copyright page) | EN | PDF | https://courses.csail.mit.edu/6.042/spring18/mcs.pdf | Discrete maths and proofs for CS. Use this PDF, not the OCW pages, which are NC-SA. |
| Calculus Made Easy | Silvanus P. Thompson (died 1916) | **PD** (US per PG; PD in EU/Spain by death date) | EN | HTML, EPUB, TXT | https://www.gutenberg.org/ebooks/33283 | Very clear calculus intuition. Old notation, so treat as historical but still usable. |
| Active Calculus (single variable; also Multivariable and Prelude) | Matt Boelkins et al. | **CC BY-SA 4.0** | EN | HTML, PDF, PreTeXt src | faculty.gvsu.edu/boelkinm/Home/ACS/colophon-1.html ; activecalculus.org ("Each text carries a Creative Commons BY-SA license") | **Best CC replacement for OpenStax Calculus.** |
| Vector Calculus | Michael Corral | **GFDL 1.2** | EN | PDF, LaTeX | https://www.mecmath.net/ | Multivariable calculus for engineers. |
| A First Course in Linear Algebra | Robert Beezer | **GFDL 1.2+** (no invariant sections) | EN | HTML, PDF, src | github.com/rbeezer/fcla (COPYING.txt); linear.pugetsound.edu | Full linear algebra course. |
| Linear Algebra | Jim Hefferon | **GFDL or CC BY-SA 3.0 US**, your choice | EN | PDF, LaTeX | https://hefferon.net/source.html | Proof-based linear algebra. |
| Understanding Linear Algebra | David Austin (GVSU) | **CC BY 4.0** | EN | HTML, PDF | https://understandinglinearalgebra.org/ | Applied, computational linear algebra. |
| Notes on Diffy Qs; Basic Analysis | Jiří Lebl | Dual **CC BY-SA 4.0** / CC BY-NC-SA 4.0 (use the BY-SA option) | EN | HTML, PDF, LaTeX | jirka.org/diffyqs ; github.com/jirilebl/ra LICENSE.md | ODEs and Laplace transforms for engineering; real analysis. |
| Introduction to Probability (Grinstead & Snell, version of 2006-07-04) | Grinstead, Snell, Doyle | **GFDL** | EN | PDF | math.dartmouth.edu/~prob/prob/prob.pdf (copyright page) | Probability foundations. |
| OpenIntro Statistics 4e; Introduction to Modern Statistics | Diez, Çetinkaya-Rundel, Barr; Çetinkaya-Rundel, Hardin | **CC BY-SA 3.0**. OpenIntro branding, logos and covers are excluded; derivatives must not be titled as OpenIntro books. | EN | PDF, src | https://www.openintro.org/license | Applied statistics. |
| Online Statistics Education | David M. Lane et al. (Rice / UHCL / Tufts) | **Public domain** ("This work is in the public domain") | EN | HTML, PDF | https://onlinestatbook.com/ | Introductory statistics with no conditions. |
| Discrete Mathematics: An Open Introduction, **3rd ed. (2019)** | Oscar Levin | **CC BY-SA 4.0** (3rd and earlier editions only) | EN | PreTeXt src, PDF (GitHub release `3rd-ed`) | github.com/oscarlevin/discrete-book LICENSE | Discrete maths. **The 4th edition is NC-SA.** |
| Statistics (high school) / Introductory Statistics 2e / Precalculus 2e | OpenStax | CC BY 4.0 (Statistics is still live; the others only via snapshot) | EN | HTML, PDF, CNXML | OpenStax section above | Stats and precalculus. |
| Introducción a la estadística; Introducción a la estadística empresarial; Precálculo 2ed | OpenStax | **CC BY 4.0** | **ES** | HTML, PDF, CNXML | OpenStax section above (repo LICENSE and collection XML) | Spanish maths and statistics. |

## 3. Medicine, nursing and health sciences

| Title | Author / publisher | Licence | Lang | Formats | Verified at | Why it's useful |
|---|---|---|---|---|---|---|
| Open RN series: Nursing Pharmacology 2e (2023), Nursing Fundamentals 2e (2024), Nursing Skills 2e (2023), Nursing Advanced Skills (2023), Nursing Mental Health & Community Concepts 2e (2025), Nursing Management & Professional Concepts 2e (2024) | Ernstmeyer & Christman (eds.), WisTech Open / Chippewa Valley Technical College | **CC BY 4.0** "except where otherwise noted" | EN | HTML, PDF, EPUB, XHTML | wtcs.pressbooks.pub/pharmacology2e/, /nursingfundamentals/, /nursingskills/, /nursingadvancedskills/, /nursingmhcc/, /nursingmpc/ | **Best CC BY nursing set.** NCLEX-aligned and current. |
| Anatomy & Physiology 2e (2025) | Biga et al., Open Oregon State (adapted from OpenStax) | **CC BY-SA 4.0** | EN | HTML, PDF, EPUB | https://open.oregonstate.education/aandp/ | Current A&P under an acceptable licence. |
| Clinical Procedures for Safer Patient Care (2015) | Doyle & McCutcheon, BCcampus / TRU | **CC BY 4.0** | EN | HTML, PDF, EPUB | https://opentextbc.ca/clinicalskills/ | Step-by-step clinical procedures. |
| Vital Sign Measurement Across the Lifespan, 1st Canadian ed. (2018) | Ryerson / TMU | **CC BY 4.0** | EN | HTML, PDF, EPUB | https://pressbooks.library.torontomu.ca/vitalsign/ | Vital-signs technique. |
| Building a Medical Terminology Foundation (2020) | Carter & Rutherford, eCampusOntario | **CC BY 4.0** | EN | HTML, PDF, EPUB | https://ecampusontario.pressbooks.pub/medicalterminology/ | Medical vocabulary, useful for query understanding. |
| Health Case Studies (2017) | BCIT / BCcampus | **CC BY-SA 4.0** | EN | HTML, PDF, EPUB | https://pressbooks.bccampus.ca/healthcasestudies/ | Clinical case reasoning. |
| Human Nutrition: 2020 Edition | Univ. of Hawaiʻi at Mānoa, Food Science & Human Nutrition | **CC BY 4.0** | EN | HTML, PDF, EPUB | https://pressbooks.oer.hawaii.edu/humannutrition2/ | Nutrition science; also fits the food section. |
| A&P 2e, Microbiology, Pharmacology for Nurses, Nutrition for Nurses, Population Health for Nurses | OpenStax | CC BY 4.0 **only at the snapshot commits** | EN | HTML, PDF, CNXML | OpenStax section above | Large, high-quality texts (2016–2024). |
| CDC "Pink Book": Epidemiology and Prevention of Vaccine-Preventable Diseases, 14th ed. (chapters updated to 2024) | CDC | **PD** (US federal). CDC asks for attribution and a no-endorsement statement, and says don't change substantive content. Contractor-made items are excluded. | EN | HTML, PDF | cdc.gov/other/agencymaterials.html ; cdc.gov/pinkbook/ | Vaccines and infectious disease. |
| NCI PDQ cancer information summaries | National Cancer Institute | **PD text**. The "PDQ" trademark and NCI logo may only appear on unchanged copies. Credit NCI and check image credits. | EN (some ES) | HTML | https://www.cancer.gov/policies/copyright-reuse | Peer-reviewed cancer summaries for patients and professionals. |
| MedlinePlus: NLM-written health-topic summaries, medical test info, genetics summaries | US National Library of Medicine | **PD** for NLM-authored content only. **The A.D.A.M. encyclopedia and ASHP drug monographs are copyrighted, so exclude them.** | EN, ES | HTML, XML | medlineplus.gov/about/using/usingcontent/ | Plain-language health content. |
| *Historical:* Notes on Nursing (1860) | Florence Nightingale (died 1910) | **PD** | EN | HTML, EPUB, TXT | https://www.gutenberg.org/ebooks/12439 | History of nursing only. Mark as historical. |
| *Historical:* Anatomy of the Human Body (Gray's, 20th ed. 1918, ed. W. H. Lewis) | Gray / Lewis | **PD in the US** (Wikisource: published before 1931). EU status depends on the rule of the shorter term, since the editor died in 1964. Flag it. | EN | HTML (Wikisource, incomplete) | en.wikisource.org/wiki/Anatomy_of_the_Human_Body | Medically **obsolete**. Historical only. |

**Gap:** I verified no Spanish-language medical text under an acceptable licence. **Caution:** medical answers need date-stamping and a disclaimer.

## 4. Cooking and food

**Open textbooks (BCcampus / go2HR / BC Cook Articulation Committee, 2015, all CC BY 4.0, all available as HTML, PDF and EPUB):**

| Title | Verified at | Why it's useful |
|---|---|---|
| Basic Kitchen and Food Service Management | https://opentextbc.ca/basickitchenandfoodservicemanagement/ | Kitchen operations. |
| Meat Cutting and Processing for Food Service | https://opentextbc.ca/meatcutting/ | Butchery. |
| Understanding Ingredients for the Canadian Baker | https://opentextbc.ca/ingredients/ | Baking science. |
| Modern Pastry and Plated Dessert Techniques | https://opentextbc.ca/modernpastryandplateddesserts/ | Pastry. |
| Nutrition and Labelling for the Canadian Baker (copyright go2HR) | https://opentextbc.ca/nutrition/ | Nutrition and labels. |
| Food Safety, Sanitation, and Personal Hygiene | https://opentextbc.ca/foodsafety/ | Food safety. |
| Workplace Safety in the Foodservice Industry | https://opentextbc.ca/workplacesafety/ | Kitchen safety. |
| Human Resources in the Food Service and Hospitality Industry | https://opentextbc.ca/humanresourcesinfoodservices/ | Restaurant management. |

**US federal:**
- **FDA Food Code (2026 edition):** **PD** under FDA policy: "contents… are not copyrighted. They are in the public domain". PDF. Verified at fda.gov/about-fda/about-website/website-policies and fda.gov/food/retail-food-protection/fda-food-code. It is the reference for safe handling and temperatures.

**Historic public-domain cookbooks (Project Gutenberg, PD in the US; all authors died long enough ago to be PD in the EU and Spain).** Mark these as historical: they contain outdated food-safety practices (preserving, raw eggs and milk, metal vessels) and should be paired with the Food Code.

| Title | Author / year | Lang | Gutenberg |
|---|---|---|---|
| The Boston Cooking-School Cook Book | Fannie Farmer (died 1915) | EN | #65061 |
| The Book of Household Management | Isabella Beeton (died 1865), 1861 | EN | #10136 |
| The Virginia Housewife | Mary Randolph (died 1828), 1824 | EN | #12519 |
| Modern Cookery for Private Families | Eliza Acton (died 1859), 1882 ed. | EN | #72482 |
| A Guide to Modern Cookery | Auguste Escoffier (died 1935), 1907 English ed. | EN | #71395 |
| La scienza in cucina e l'arte di mangiar bene | Pellegrino Artusi (died 1911) | IT | #59047 |

All are available as HTML, EPUB and TXT at gutenberg.org/ebooks/<number>.

**Spanish public-domain cookbooks (verified):**

| Title | Author / year | PD basis | Formats | Verified at |
|---|---|---|---|---|
| Cocina moderna: tratado completo de cocina, pastelería, repostería y botillería | Anonymous, Madrid 1875 (digitised from the BNE) | Anonymous, 1875 | HTML, EPUB, TXT | gutenberg.org/ebooks/45770 (date from title page) |
| Almacén de recetas escogidas y probadas (syrups, liqueurs, ices) | Anonymous, Barcelona 1840 | Anonymous, 1840 | HTML, EPUB, TXT | gutenberg.org/ebooks/43276 |
| Cocina cómica: recetas de guisos y postres… | Juan Pérez Zúñiga, 1897 (died 1938) | PD in Spain since 2019 (death + 80) | HTML, EPUB, TXT | gutenberg.org/ebooks/40595 ; es.wikipedia (death date) |
| Nuevo arte de cocina | Anonymous, 1864 | Wikisource tag "100 p.a.p.", validated text | HTML | es.wikisource.org/wiki/Nuevo_arte_de_cocina |
| Cocina ecléctica | Juana Manuela Gorriti, Buenos Aires 1890 | Wikisource tag "100 p.m.a." | HTML | es.wikisource.org/wiki/Cocina_ecléctica |

There is one more possible Spanish title, **La mejor cocinera** (Calleja, Gutenberg #8870). It is PD in the US, but the text carries no date, so its Spanish status is likely PD but unconfirmed. Check the year before relying on it.

## 5. Civil engineering

These are mostly US federal works. Basis for each:
- **US Bureau of Reclamation (USBR, part of the Department of the Interior):** DOI says "materials produced by federal agencies are in the public domain" (https://www.doi.gov/copyright). Manual index: https://www.usbr.gov/tsc/techreferences/mands/manuals.html.
- **FHWA:** "Information presented on this web site is considered public information and may be distributed or copied" (https://www.fhwa.dot.gov/disclaim.cfm).
- **USGS:** "USGS-authored or produced data and information are considered to be in the U.S. Public Domain" (usgs.gov copyrights-and-credits).

| Title | Publisher | Licence | Formats | Verified at | Why it's useful / flags |
|---|---|---|---|---|---|
| Design of Small Dams, 3rd ed. (1987) | USBR | PD (US federal) | PDF | USBR manuals page | Embankment and concrete dams, spillways, hydrology. **Dated, so check against current codes.** |
| Earth Manual, Parts 1 & 2 (3rd ed.) | USBR | PD | PDF | same | Soil testing and classification (geotechnics). |
| Design of Small Canal Structures | USBR | PD | PDF | same | Open-channel structures. Dated. |
| Engineering Geology Field Manual | USBR | PD | PDF | same | Site investigation. |
| Water Measurement Manual (3rd ed. 1997, rev. 2001) | USBR | PD | HTML, PDF | usbr.gov/tsc/techreferences/mands/wmm.html | Weirs, flumes, flow measurement. |
| Concrete Manual (8th ed. rev.); Design of Gravity Dams; Design of Arch Dams | USBR | PD | PDF | USBR manuals page | Concrete and structures. **Old, so mark as historical reference.** |
| HDS-5 Hydraulic Design of Highway Culverts (3rd ed.); HEC-14 Energy Dissipators; HEC-18 Evaluating Scour at Bridges (5th ed.); HEC-22 Urban Drainage Design Manual (4th ed.); HEC-23 Bridge Scour Countermeasures (3rd ed.) | FHWA | Public information, may be copied (agency statement). Many were written by contractors, so check for third-party figures. | PDF | fhwa.dot.gov/engineering/hydraulics/library_listing.cfm | Core highway hydraulics and drainage. |
| HEC-RAS Hydraulic Reference Manual v6.6 (2024) | USACE Hydrologic Engineering Center | US federal; marked "Approved for Public Release. Distribution Unlimited." The terms of use cover the software only, and the "HEC-RAS" name can't be used for derivatives. | PDF, HTML | hec.usace.army.mil/software/hec-ras/documentation.aspx (PDF front matter) | Open-channel and unsteady-flow theory. |
| WSP 2339 Guide for selecting Manning's roughness coefficients (1989); WSP 1849 Roughness characteristics of natural channels (1967); TM 3-A8 Discharge measurements at gaging stations (2010); TWRI 3-A1 Indirect discharge measurements (1967) | USGS | PD | PDF | pubs.usgs.gov/publication/wsp2339, /wsp1849, /tm3A8, /twri03A1 | Hydraulics field practice. |
| Wood Handbook: Wood as an Engineering Material (FPL-GTR-282, 2021) | USDA Forest Products Laboratory | US federal work. The record has no licence line; status rests on FPL authorship plus the Forest Service statement that USDA employee works are PD under §105. | PDF (104 MB) | research.fs.usda.gov/treesearch/62200 | Timber properties and design. |
| FEMA P-749 Earthquake-Resistant Design Concepts, 2nd ed. (2022) | FEMA (prepared by ATC under contract) | FEMA: "Most material on FEMA.gov is free of copyright…" **Flag: contractor-prepared**, so check credits. | PDF | fema.gov/about/website-information ; wbdg.org/FFC/DHS/fema_p749_2022.pdf | Seismic design concepts. |
| University Physics Vol. 1 (EN snapshot) / Física universitaria vol. 1 (ES, CC BY) | OpenStax | CC BY 4.0 | HTML, PDF | OpenStax section above | Statics, equilibrium, elasticity, fluids. |

**Gap:** I found **no** statics or mechanics-of-materials textbook under CC BY or CC BY-SA (see the exclusions). The federal manuals plus University Physics are the workable base.

## 6. General science foundations

| Title | Author / publisher | Licence | Lang | Formats | Verified at | Why it's useful |
|---|---|---|---|---|---|---|
| University Physics 1–3, College Physics 2e, Chemistry 2e, Chemistry: Atoms First 2e, Biology 2e, Concepts of Biology, Astronomy 2e | OpenStax | CC BY 4.0 **at the snapshot commits** | EN | HTML, PDF, CNXML | OpenStax section above | Full intro-science coverage. |
| Physics (high school) | OpenStax / Texas Education Agency | **CC BY 4.0** (still live) | EN | HTML, PDF | OpenStax CMS plus osbooks-physics LICENSE | Basic physics. |
| Física universitaria 1–3; Química 2ed; Química: Comenzando con los átomos 2ed | OpenStax | **CC BY 4.0** (still live; pin the commit) | **ES** | HTML, PDF, CNXML | osbooks-fisica-universitaria-bundle, osbooks-quimica-bundle (LICENSE and collection XML) | Main Spanish science base. |
| Light and Matter series; Simple Nature | Benjamin Crowell | **CC BY-SA 3.0 US** (site footer; photo licences vary, see credits) | EN | PDF, LaTeX src | http://www.lightandmatter.com/ | Physics with calculus. |
| Concepts of Biology, 1st Canadian ed. (2015) | Molnar & Gair, BCcampus | **CC BY 4.0** | EN | HTML, PDF, EPUB | https://opentextbc.ca/biology/ | Intro biology. |
| Physical Geology, 2nd ed. (2019) | Steven Earle, BCcampus | **CC BY 4.0** | EN | HTML, PDF, EPUB | https://opentextbc.ca/physicalgeology2ed/ | Geology, also background for geotechnics. |

---

## Excluded and why (licences verified)

| Item | Licence / reason |
|---|---|
| OpenStax current English editions (all live books except *Physics* and *Statistics*) | CC BY-NC-SA 4.0 since 2026-03. Only the snapshots above are usable. |
| OpenStax Calculus vol. 1–3 and Spanish *Cálculo* vol. 1–3 | CC BY-NC-SA 4.0. |
| OpenStax Introduction to Computer Science; Clinical Nursing Skills; Fundamentals of Nursing; Medical-Surgical; Maternal-Newborn; Psychiatric-Mental Health Nursing | NC-SA, and I found no CC BY snapshot. |
| Engineering Statics: Open and Interactive (Baker & Haynes) | CC BY-NC-SA (PDF front matter). |
| Structural Analysis (Udoeyo, Temple University Press) | CC BY-NC-ND. |
| Introductory Chemistry, 1st Canadian ed. (BCcampus) | CC BY-NC-SA 4.0. |
| Discrete Mathematics: An Open Introduction, 4th ed. | CC BY-NC-SA 4.0. The 3rd edition is fine. |
| Erickson's other lecture notes (not the book) | CC BY-NC-SA 4.0. |
| Pro Git | CC BY-NC-SA 3.0. |
| Think Python (3e) | CC BY-NC-SA 4.0. |
| Python for Everybody (the book) | CC BY-NC-SA 3.0 (book copyright page, even though the site footer says CC BY). |
| Eloquent JavaScript | CC BY-NC 3.0 (code MIT). |
| Crafting Interpreters | Prose CC BY-NC-ND 4.0 (code MIT). |
| StatPearls (NCBI Bookshelf) | CC BY-NC-ND 4.0. |
| WHO publications | Generally CC BY-NC-SA 3.0 IGO. |
| MedlinePlus A.D.A.M. encyclopedia; ASHP drug monographs | Copyrighted. |
| Cocina del tiempo (Luis Ruiz Contreras, died 1953; Gutenberg #42642) | PD in the US but **protected in Spain until the end of 2033** (death + 80). |

## Left out because their licence could not be verified

- **Mechanics Map (Moore):** says only "a Creative Commons Share-alike license", with no version and no word on NC.
- **Basics of Fluid Mechanics (Bar-Meir, Potto Project):** the Open Textbook Library lists it as GFDL, but the Zenodo record for v0.7.5 lists GPL-3.0-or-later. Both are free licences; check the PDF's copyright page before including it.
- **NIST/SEMATECH e-Handbook of Statistical Methods:** joint work with SEMATECH and no licence statement.
- **USACE Engineer Manuals (EM 1110 series):** probably PD as federal-employee works, but the site blocked automated access.
- **NRCS National Engineering Handbook:** site timed out.
- **USDA Complete Guide to Home Canning:** hosted by UGA's NCHFP; I couldn't confirm federal authorship.
- **La mejor cocinera (Calleja):** the text carries no date, so its Spanish status is unconfirmed.
