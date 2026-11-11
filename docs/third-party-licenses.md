# 依存クレートのライセンス

`wys` が使う Rust のクレートと、そのライセンスの一覧（ADR-0028）。`scripts/third-party-licenses.py` が
`Cargo.lock` から作る（手で編集しない。`scripts/ci.sh` が最新かを確かめる）。開発とビルドにだけ使うものも含む。
「ランタイム」に ○ があるものは、利用者のプログラムの実行ファイルに静的リンクされる。各ライセンスの全文は
それぞれのクレートの配布物にある。

クレートの数: 151

| クレート | 版 | ライセンス | ランタイム | リポジトリ |
|---|---|---|:-:|---|
| `adler2` | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |  | https://github.com/oyvindln/adler2 |
| `ahash` | 0.8.12 | MIT OR Apache-2.0 |  | https://github.com/tkaitchuck/ahash |
| `aho-corasick` | 1.1.5 | Unlicense OR MIT |  | https://github.com/BurntSushi/aho-corasick |
| `autocfg` | 1.5.1 | Apache-2.0 OR MIT |  | https://github.com/cuviper/autocfg |
| `base64` | 0.22.1 | MIT OR Apache-2.0 |  | https://github.com/marshallpierce/rust-base64 |
| `bit-set` | 0.8.0 | Apache-2.0 OR MIT |  | https://github.com/contain-rs/bit-set |
| `bit-vec` | 0.8.0 | Apache-2.0 OR MIT |  | https://github.com/contain-rs/bit-vec |
| `bitflags` | 1.3.2 | MIT/Apache-2.0 |  | https://github.com/bitflags/bitflags |
| `bitflags` | 2.13.2 | MIT OR Apache-2.0 |  | https://github.com/bitflags/bitflags |
| `borrow-or-share` | 0.2.4 | MIT-0 |  | https://github.com/yescallop/borrow-or-share |
| `bumpalo` | 3.20.3 | MIT OR Apache-2.0 |  | https://github.com/fitzgen/bumpalo |
| `bytecount` | 0.6.9 | Apache-2.0/MIT |  | https://github.com/llogiq/bytecount |
| `cc` | 1.11.0 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/cc-rs |
| `cfg-if` | 1.0.5 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/cfg-if |
| `console` | 0.16.6 | MIT |  | https://github.com/console-rs/console |
| `crc32fast` | 1.5.4 | MIT OR Apache-2.0 |  | https://github.com/srijs/rust-crc32fast |
| `crossbeam-channel` | 0.5.17 | MIT OR Apache-2.0 |  | https://github.com/crossbeam-rs/crossbeam |
| `crossbeam-utils` | 0.8.23 | MIT OR Apache-2.0 |  | https://github.com/crossbeam-rs/crossbeam |
| `displaydoc` | 0.2.7 | MIT OR Apache-2.0 |  | https://github.com/yaahc/displaydoc |
| `email_address` | 0.2.9 | MIT |  | https://github.com/johnstonskj/rust-email_address.git |
| `encode_unicode` | 1.0.0 | Apache-2.0 OR MIT |  | https://github.com/tormol/encode_unicode |
| `errno` | 0.3.14 | MIT OR Apache-2.0 |  | https://github.com/lambda-fairy/rust-errno |
| `fancy-regex` | 0.14.0 | MIT |  | https://github.com/fancy-regex/fancy-regex |
| `fastrand` | 2.5.0 | Apache-2.0 OR MIT |  | https://github.com/smol-rs/fastrand |
| `find-msvc-tools` | 0.1.15 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/cc-rs |
| `flate2` | 1.1.10 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/flate2-rs |
| `fluent-uri` | 0.1.4 | MIT |  | https://github.com/yescallop/fluent-uri-rs |
| `fluent-uri` | 0.3.2 | MIT |  | https://github.com/yescallop/fluent-uri-rs |
| `form_urlencoded` | 1.2.2 | MIT OR Apache-2.0 |  | https://github.com/servo/rust-url |
| `fraction` | 0.15.4 | MIT OR Apache-2.0 |  | https://github.com/dnsl48/fraction.git |
| `futures-core` | 0.3.34 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/futures-rs |
| `futures-task` | 0.3.34 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/futures-rs |
| `futures-util` | 0.3.34 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/futures-rs |
| `getrandom` | 0.2.17 | MIT OR Apache-2.0 |  | https://github.com/rust-random/getrandom |
| `getrandom` | 0.3.4 | MIT OR Apache-2.0 |  | https://github.com/rust-random/getrandom |
| `icu_collections` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `icu_locale_core` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `icu_normalizer` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `icu_normalizer_data` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `icu_properties` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `icu_properties_data` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `icu_provider` | 2.4.0 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `idna` | 1.1.0 | MIT OR Apache-2.0 |  | https://github.com/servo/rust-url/ |
| `idna_adapter` | 1.2.2 | Apache-2.0 OR MIT |  | https://github.com/hsivonen/idna_adapter |
| `insta` | 1.49.0 | Apache-2.0 |  | https://github.com/mitsuhiko/insta |
| `itoa` | 1.0.18 | MIT OR Apache-2.0 | ○ | https://github.com/dtolnay/itoa |
| `js-sys` | 0.3.107 | MIT OR Apache-2.0 |  | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys |
| `jsonschema` | 0.30.0 | MIT |  | https://github.com/Stranger6667/jsonschema |
| `lazy_static` | 1.5.1 | MIT OR Apache-2.0 |  | https://github.com/rust-lang-nursery/lazy-static.rs |
| `libc` | 0.2.191 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/libc |
| `linux-raw-sys` | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  | https://github.com/sunfishcode/linux-raw-sys |
| `litemap` | 0.8.3 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `lock_api` | 0.4.14 | MIT OR Apache-2.0 |  | https://github.com/Amanieu/parking_lot |
| `log` | 0.4.35 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/log |
| `lsp-server` | 0.7.9 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/rust-analyzer/tree/master/lib/lsp-server |
| `lsp-types` | 0.97.0 | MIT |  | https://github.com/gluon-lang/lsp-types |
| `memchr` | 2.8.3 | Unlicense OR MIT | ○ | https://github.com/BurntSushi/memchr |
| `miniz_oxide` | 0.9.1 | MIT OR Zlib OR Apache-2.0 |  | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| `num` | 0.4.3 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num |
| `num-bigint` | 0.4.8 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num-bigint |
| `num-cmp` | 0.1.0 | MIT/Apache-2.0 |  | https://github.com/lifthrasiir/num-cmp |
| `num-complex` | 0.4.6 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num-complex |
| `num-integer` | 0.1.47 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num-integer |
| `num-iter` | 0.1.46 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num-iter |
| `num-rational` | 0.4.2 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num-rational |
| `num-traits` | 0.2.19 | MIT OR Apache-2.0 |  | https://github.com/rust-num/num-traits |
| `once_cell` | 1.21.4 | MIT OR Apache-2.0 |  | https://github.com/matklad/once_cell |
| `outref` | 0.5.2 | MIT |  | https://github.com/Nugine/outref |
| `parking_lot` | 0.12.5 | MIT OR Apache-2.0 |  | https://github.com/Amanieu/parking_lot |
| `parking_lot_core` | 0.9.12 | MIT OR Apache-2.0 |  | https://github.com/Amanieu/parking_lot |
| `percent-encoding` | 2.3.2 | MIT OR Apache-2.0 |  | https://github.com/servo/rust-url/ |
| `pin-project-lite` | 0.2.17 | Apache-2.0 OR MIT |  | https://github.com/taiki-e/pin-project-lite |
| `potential_utf` | 0.1.6 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `proc-macro2` | 1.0.107 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/proc-macro2 |
| `quote` | 1.0.47 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/quote |
| `r-efi` | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later |  | https://github.com/r-efi/r-efi |
| `redox_syscall` | 0.5.18 | MIT |  | https://gitlab.redox-os.org/redox-os/syscall |
| `ref-cast` | 1.0.27 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/ref-cast |
| `ref-cast-impl` | 1.0.27 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/ref-cast |
| `referencing` | 0.30.0 | MIT |  | https://github.com/Stranger6667/jsonschema |
| `regex` | 1.13.1 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/regex |
| `regex-automata` | 0.4.18 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/regex |
| `regex-syntax` | 0.8.11 | MIT OR Apache-2.0 |  | https://github.com/rust-lang/regex |
| `ring` | 0.17.14 | Apache-2.0 AND ISC |  | https://github.com/briansmith/ring |
| `rustix` | 1.1.6 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  | https://github.com/bytecodealliance/rustix |
| `rustls` | 0.23.46 | Apache-2.0 OR ISC OR MIT |  | https://github.com/rustls/rustls |
| `rustls-pki-types` | 1.15.1 | MIT OR Apache-2.0 |  | https://github.com/rustls/pki-types |
| `rustls-webpki` | 0.103.16 | ISC |  | https://github.com/rustls/webpki |
| `rustversion` | 1.0.23 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/rustversion |
| `scopeguard` | 1.2.0 | MIT OR Apache-2.0 |  | https://github.com/bluss/scopeguard |
| `serde` | 1.0.229 | MIT OR Apache-2.0 | ○ | https://github.com/serde-rs/serde |
| `serde_core` | 1.0.229 | MIT OR Apache-2.0 | ○ | https://github.com/serde-rs/serde |
| `serde_derive` | 1.0.229 | MIT OR Apache-2.0 |  | https://github.com/serde-rs/serde |
| `serde_json` | 1.0.154 | MIT OR Apache-2.0 | ○ | https://github.com/serde-rs/json |
| `serde_repr` | 0.1.21 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/serde-repr |
| `shlex` | 2.0.1 | MIT OR Apache-2.0 |  | https://github.com/comex/rust-shlex |
| `simd-adler32` | 0.3.10 | MIT |  | https://github.com/mcountryman/simd-adler32 |
| `similar` | 2.7.0 | Apache-2.0 |  | https://github.com/mitsuhiko/similar |
| `slab` | 0.4.12 | MIT |  | https://github.com/tokio-rs/slab |
| `smallvec` | 1.16.4 | MIT OR Apache-2.0 |  | https://github.com/servo/rust-smallvec |
| `stable_deref_trait` | 1.2.1 | MIT OR Apache-2.0 |  | https://github.com/storyyeller/stable_deref_trait |
| `subtle` | 2.6.1 | BSD-3-Clause |  | https://github.com/dalek-cryptography/subtle |
| `syn` | 2.0.119 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/syn |
| `syn` | 3.0.9 | MIT OR Apache-2.0 |  | https://github.com/dtolnay/syn |
| `synstructure` | 0.14.0 | MIT |  | https://github.com/mystor/synstructure |
| `tempfile` | 3.27.0 | MIT OR Apache-2.0 |  | https://github.com/Stebalien/tempfile |
| `tinystr` | 0.8.5 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `unicode-ident` | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 |  | https://github.com/dtolnay/unicode-ident |
| `untrusted` | 0.9.0 | ISC |  | https://github.com/briansmith/untrusted |
| `ureq` | 2.12.1 | MIT OR Apache-2.0 |  | https://github.com/algesten/ureq |
| `url` | 2.5.8 | MIT OR Apache-2.0 |  | https://github.com/servo/rust-url |
| `utf8_iter` | 1.1.0 | Apache-2.0 OR MIT |  | https://github.com/hsivonen/utf8_iter |
| `utf_types` | 1.0.0 | Apache-2.0 OR MIT |  | https://github.com/hsivonen/utf_types |
| `uuid` | 1.29.0 | Apache-2.0 OR MIT |  | https://github.com/uuid-rs/uuid |
| `uuid-simd` | 0.8.0 | MIT |  | https://github.com/Nugine/simd |
| `version_check` | 0.9.5 | MIT/Apache-2.0 |  | https://github.com/SergioBenitez/version_check |
| `vsimd` | 0.8.0 | MIT |  | https://github.com/Nugine/simd |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  | https://github.com/bytecodealliance/wasi |
| `wasip2` | 1.0.4+wasi-0.2.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  | https://github.com/bytecodealliance/wasi-rs |
| `wasm-bindgen` | 0.2.130 | MIT OR Apache-2.0 |  | https://github.com/wasm-bindgen/wasm-bindgen |
| `wasm-bindgen-macro` | 0.2.130 | MIT OR Apache-2.0 |  | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro |
| `wasm-bindgen-macro-support` | 0.2.130 | MIT OR Apache-2.0 |  | https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support |
| `wasm-bindgen-shared` | 0.2.130 | MIT OR Apache-2.0 |  | https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared |
| `webpki-roots` | 0.26.11 | CDLA-Permissive-2.0 |  | https://github.com/rustls/webpki-roots |
| `webpki-roots` | 1.0.11 | CDLA-Permissive-2.0 |  | https://github.com/rustls/webpki-roots |
| `windows-link` | 0.2.1 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows-sys` | 0.52.0 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows-sys` | 0.61.2 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows-targets` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_aarch64_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_aarch64_msvc` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_i686_gnu` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_i686_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_i686_msvc` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_x86_64_gnu` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_x86_64_gnullvm` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `windows_x86_64_msvc` | 0.52.6 | MIT OR Apache-2.0 |  | https://github.com/microsoft/windows-rs |
| `wit-bindgen` | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |  | https://github.com/bytecodealliance/wit-bindgen |
| `writeable` | 0.6.4 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `yoke` | 0.8.4 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `yoke-derive` | 0.8.4 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `zerocopy` | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT |  | https://github.com/google/zerocopy |
| `zerocopy-derive` | 0.8.63 | BSD-2-Clause OR Apache-2.0 OR MIT |  | https://github.com/google/zerocopy |
| `zerofrom` | 0.1.8 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `zerofrom-derive` | 0.1.8 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `zeroize` | 1.9.1 | Apache-2.0 OR MIT |  | https://github.com/RustCrypto/utils |
| `zerotrie` | 0.2.5 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `zerovec` | 0.11.9 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `zerovec-derive` | 0.11.7 | Unicode-3.0 |  | https://github.com/unicode-org/icu4x |
| `zlib-rs` | 0.6.8 | Zlib |  | https://github.com/trifectatechfoundation/zlib-rs |
| `zmij` | 1.0.23 | MIT | ○ | https://github.com/dtolnay/zmij |
