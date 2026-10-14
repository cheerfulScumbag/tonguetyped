//! Reviewed catalog of GGUF speech-recognition models published by
//! [handy-computer](https://huggingface.co/handy-computer), derived from
//! [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp)'s release
//! `catalog.db` (see `data/tt-model-catalog-19/report.md` for the derivation and
//! verification process). It covers the offline, text-producing families this
//! project's VAD-then-single-buffer pipeline has been checked against
//! (`src/coordinator.rs` filters the recording with Silero VAD and hands the
//! engine one buffer): whisper, parakeet, moonshine, canary, and cohere. Each
//! entry's family, language coverage, and long-form behavior live in
//! `VARIANTS`, keyed by the transcribe.cpp `variant` slug.
//!
//! Families are admitted only after their output and chunking semantics have
//! been checked against that single-buffer pipeline. Each family falls into
//! one long-form bucket from transcribe.cpp's input-length contract
//! (`docs/input-limits.md`), recorded as [`LongForm`]:
//!
//! - whisper and parakeet window or encode internally and accept any length
//!   ([`LongForm::Unbounded`]).
//! - moonshine accepts any length but can truncate its output past roughly 48
//!   seconds ([`LongForm::SoftWindow`]); the transcript comes back short, not
//!   as an error.
//! - canary and cohere reject input past their encoder table (about 400
//!   seconds) before decoding ([`LongForm::HardCap`]), so
//!   [`crate::config::Config::validate`] refuses a `max_recording_seconds`
//!   above the active model's cap.
//!
//! Diarization (`sortformer`) and streaming-only families stay out: neither
//! produces a single offline transcript through `Session::run`.
//!
//! Entries were derived by reading the transcribe.cpp `catalog/*.json` records
//! (family, languages, downloads) and resolving each published HuggingFace
//! file's pinned commit and `X-Linked-ETag` (the file's SHA-256). Follow the
//! same steps to add or repin an entry rather than trusting an unpinned URL.

/// What a family's GGUF gives as its single-buffer input ceiling, so the
/// catalog and config validation reason about the recording-length limit
/// instead of the old whisper-only assumption. Mirrors transcribe.cpp's
/// input-length contract; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongForm {
    /// Any audio length is accepted: the family windows internally (whisper)
    /// or its encoder is unbounded (parakeet).
    Unbounded,
    /// Any length is accepted, but past `window_seconds` the output can be
    /// truncated rather than the call rejected (moonshine).
    SoftWindow { window_seconds: u32 },
    /// Input longer than `cap_seconds` is rejected before decoding (canary,
    /// cohere). The pipeline must not submit a longer buffer.
    HardCap { cap_seconds: u32 },
}

/// Family, language coverage, and long-form behavior shared by every
/// quantization of one transcribe.cpp variant. Languages and long-form
/// behavior belong to the model rather than to a particular GGUF file, so
/// they live here once instead of on every [`CatalogModel`] entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantInfo {
    /// transcribe.cpp variant slug, e.g. "whisper-small" or "parakeet-tdt-0.6b-v2".
    pub variant: &'static str,
    /// transcribe.cpp loader family (the GGUF's `general.architecture`), e.g.
    /// "whisper" or "parakeet".
    pub family: &'static str,
    /// Languages the port transcribes, in the spellings it advertises.
    pub languages: &'static [&'static str],
    pub long_form: LongForm,
}

impl VariantInfo {
    /// One-line language coverage for the model list and dashboard: "English",
    /// "N languages", or "n/a" when a port advertises none.
    pub fn language_summary(&self) -> String {
        match self.languages {
            [] => "n/a".to_string(),
            ["en"] => "English".to_string(),
            languages => format!("{} languages", languages.len()),
        }
    }

    /// Whether more than one language is advertised, so the UI can tell a
    /// multilingual model from an English-only one at a glance.
    pub fn is_multilingual(&self) -> bool {
        self.languages.len() > 1
    }

    /// Human-readable family name for the model list and dashboard. The stored
    /// `family` is the GGUF architecture (`cohere_asr`), which is what the
    /// loader keys on but not what a user recognizes.
    pub fn family_label(&self) -> &'static str {
        match self.family {
            "whisper" => "Whisper",
            "parakeet" => "Parakeet",
            "moonshine" => "Moonshine",
            "canary" => "Canary",
            "cohere_asr" => "Cohere",
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogModel {
    /// Stable identifier used in config and the `model` CLI, e.g. "whisper-small-q5_k_m".
    pub id: &'static str,
    /// transcribe.cpp catalog variant name, e.g. "whisper-small".
    pub variant: &'static str,
    pub display_name: &'static str,
    /// Quantization label as published, e.g. "Q5_K_M".
    pub quant: &'static str,
    /// Upstream model this GGUF was converted from.
    pub upstream_repo: &'static str,
    pub license_spdx: &'static str,
    /// HuggingFace repo hosting the GGUF file.
    pub hf_repo: &'static str,
    /// Commit this entry is pinned to (not "main"), so a later push to the
    /// HuggingFace repo cannot silently change what a pinned catalog entry resolves to.
    pub revision: &'static str,
    pub filename: &'static str,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the file contents, verified against HuggingFace's
    /// `X-Linked-ETag` for the pinned revision at derivation time.
    pub sha256: &'static str,
}

impl CatalogModel {
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.hf_repo, self.revision, self.filename
        )
    }

    /// This entry's family, language coverage, and long-form behavior. Every
    /// entry's variant has a [`VariantInfo`] (the
    /// `every_entrys_variant_has_capability_info` test asserts it), so this
    /// never falls back to a placeholder.
    pub fn info(&self) -> &'static VariantInfo {
        variant_info(self.variant)
            .unwrap_or_else(|| panic!("catalog variant has no VariantInfo: {}", self.variant))
    }
}

pub const ENTRIES: &[CatalogModel] = &[
    CatalogModel {
        id: "breeze-asr-25-bf16",
        variant: "breeze-asr-25",
        display_name: "breeze-asr-25",
        quant: "BF16",
        upstream_repo: "MediaTek-Research/Breeze-ASR-25",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/Breeze-ASR-25-gguf",
        revision: "6b7a53cea9265a2cf2b19e37b3ceadeb2927d3ba",
        filename: "Breeze-ASR-25-BF16.gguf",
        size_bytes: 3096013408,
        sha256: "b2ae64afd2bea002c0188bba34e868f208eca8ab2b5d1359bd5d4b052209851e",
    },
    CatalogModel {
        id: "breeze-asr-25-f16",
        variant: "breeze-asr-25",
        display_name: "breeze-asr-25",
        quant: "F16",
        upstream_repo: "MediaTek-Research/Breeze-ASR-25",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/Breeze-ASR-25-gguf",
        revision: "6b7a53cea9265a2cf2b19e37b3ceadeb2927d3ba",
        filename: "Breeze-ASR-25-F16.gguf",
        size_bytes: 3106458208,
        sha256: "1ed33e23e1dd7183c7d60e8713d159a61bfa1852019e7a65fd04ec6042a44f98",
    },
    CatalogModel {
        id: "breeze-asr-25-q4_k_m",
        variant: "breeze-asr-25",
        display_name: "breeze-asr-25",
        quant: "Q4_K_M",
        upstream_repo: "MediaTek-Research/Breeze-ASR-25",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/Breeze-ASR-25-gguf",
        revision: "6b7a53cea9265a2cf2b19e37b3ceadeb2927d3ba",
        filename: "Breeze-ASR-25-Q4_K_M.gguf",
        size_bytes: 996526080,
        sha256: "cb056e63e5606284637c8a9c6d35e3c544f190dd5d26335466c7e584e61815c4",
    },
    CatalogModel {
        id: "breeze-asr-25-q5_k_m",
        variant: "breeze-asr-25",
        display_name: "breeze-asr-25",
        quant: "Q5_K_M",
        upstream_repo: "MediaTek-Research/Breeze-ASR-25",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/Breeze-ASR-25-gguf",
        revision: "6b7a53cea9265a2cf2b19e37b3ceadeb2927d3ba",
        filename: "Breeze-ASR-25-Q5_K_M.gguf",
        size_bytes: 1160366080,
        sha256: "c871fc811b33a16a5607c4d4166cfa2c0a1d359f7796e3da255e3e922f59139b",
    },
    CatalogModel {
        id: "breeze-asr-25-q6_k",
        variant: "breeze-asr-25",
        display_name: "breeze-asr-25",
        quant: "Q6_K",
        upstream_repo: "MediaTek-Research/Breeze-ASR-25",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/Breeze-ASR-25-gguf",
        revision: "6b7a53cea9265a2cf2b19e37b3ceadeb2927d3ba",
        filename: "Breeze-ASR-25-Q6_K.gguf",
        size_bytes: 1296353280,
        sha256: "4c7fe8eed7b36a00dc345e9825fb0f4ac717c38cc5a9292cd552c2f141e3b258",
    },
    CatalogModel {
        id: "breeze-asr-25-q8_0",
        variant: "breeze-asr-25",
        display_name: "breeze-asr-25",
        quant: "Q8_0",
        upstream_repo: "MediaTek-Research/Breeze-ASR-25",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/Breeze-ASR-25-gguf",
        revision: "6b7a53cea9265a2cf2b19e37b3ceadeb2927d3ba",
        filename: "Breeze-ASR-25-Q8_0.gguf",
        size_bytes: 1667964224,
        sha256: "1650c163cd1623d13b585368ffd87d7e669c5ff23014511e0fa91bb4ce994756",
    },
    CatalogModel {
        id: "canary-1b-v2-f16",
        variant: "canary-1b-v2",
        display_name: "canary-1b-v2",
        quant: "F16",
        upstream_repo: "nvidia/canary-1b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/canary-1b-v2-gguf",
        revision: "e2d8e6d7f2accc1259dc5497b517b4083047e44b",
        filename: "canary-1b-v2-F16.gguf",
        size_bytes: 1966111456,
        sha256: "eadda53cd1652d65cd12ff7ac4b7dc64cba1ce9837aae0c86e4222e8db89e320",
    },
    CatalogModel {
        id: "canary-1b-v2-f32",
        variant: "canary-1b-v2",
        display_name: "canary-1b-v2",
        quant: "F32",
        upstream_repo: "nvidia/canary-1b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/canary-1b-v2-gguf",
        revision: "e2d8e6d7f2accc1259dc5497b517b4083047e44b",
        filename: "canary-1b-v2-F32.gguf",
        size_bytes: 3920657120,
        sha256: "b0b48b62b2c68f2779c49481de102715c5eaadbe084bca7fdddc7912f9c20e8b",
    },
    CatalogModel {
        id: "canary-1b-v2-q4_k_m",
        variant: "canary-1b-v2",
        display_name: "canary-1b-v2",
        quant: "Q4_K_M",
        upstream_repo: "nvidia/canary-1b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/canary-1b-v2-gguf",
        revision: "e2d8e6d7f2accc1259dc5497b517b4083047e44b",
        filename: "canary-1b-v2-Q4_K_M.gguf",
        size_bytes: 735476448,
        sha256: "49e0a67e219bec95a254c2348460b6350e75a7ac6f93a131e48244b4c7cb53b9",
    },
    CatalogModel {
        id: "canary-1b-v2-q5_k_m",
        variant: "canary-1b-v2",
        display_name: "canary-1b-v2",
        quant: "Q5_K_M",
        upstream_repo: "nvidia/canary-1b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/canary-1b-v2-gguf",
        revision: "e2d8e6d7f2accc1259dc5497b517b4083047e44b",
        filename: "canary-1b-v2-Q5_K_M.gguf",
        size_bytes: 836664032,
        sha256: "9c3a893c93795438baf9b4b1c853c39b60316c3a0d259a3ba6e284712f5ddb71",
    },
    CatalogModel {
        id: "canary-1b-v2-q6_k",
        variant: "canary-1b-v2",
        display_name: "canary-1b-v2",
        quant: "Q6_K",
        upstream_repo: "nvidia/canary-1b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/canary-1b-v2-gguf",
        revision: "e2d8e6d7f2accc1259dc5497b517b4083047e44b",
        filename: "canary-1b-v2-Q6_K.gguf",
        size_bytes: 931986144,
        sha256: "6567c2d601fccacf9809fc230edefde05728340f65860bbe91cfd72d0a5eea9d",
    },
    CatalogModel {
        id: "canary-1b-v2-q8_0",
        variant: "canary-1b-v2",
        display_name: "canary-1b-v2",
        quant: "Q8_0",
        upstream_repo: "nvidia/canary-1b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/canary-1b-v2-gguf",
        revision: "e2d8e6d7f2accc1259dc5497b517b4083047e44b",
        filename: "canary-1b-v2-Q8_0.gguf",
        size_bytes: 1144290016,
        sha256: "224f83d1bc487b3303b495a7d6874912fdece93de19d1a04b550829c30a5d289",
    },
    CatalogModel {
        id: "cohere-transcribe-03-2026-bf16",
        variant: "cohere-transcribe-03-2026",
        display_name: "cohere-transcribe-03-2026",
        quant: "BF16",
        upstream_repo: "CohereLabs/cohere-transcribe-03-2026",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/cohere-transcribe-03-2026-gguf",
        revision: "ab667acedcb5d8d56837ef3ed3568c3ad50dde47",
        filename: "cohere-transcribe-03-2026-BF16.gguf",
        size_bytes: 4105263104,
        sha256: "72e7625abc358da7b1a27348b18269b4a026eceb792e6ade53c6bb2280d0f3a7",
    },
    CatalogModel {
        id: "cohere-transcribe-03-2026-f16",
        variant: "cohere-transcribe-03-2026",
        display_name: "cohere-transcribe-03-2026",
        quant: "F16",
        upstream_repo: "CohereLabs/cohere-transcribe-03-2026",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/cohere-transcribe-03-2026-gguf",
        revision: "ab667acedcb5d8d56837ef3ed3568c3ad50dde47",
        filename: "cohere-transcribe-03-2026-F16.gguf",
        size_bytes: 4106644992,
        sha256: "65ea095ba78ed938a613cc950da0599b29e16bb5a22a968d7b1fd99d71f13784",
    },
    CatalogModel {
        id: "cohere-transcribe-03-2026-q4_k_m",
        variant: "cohere-transcribe-03-2026",
        display_name: "cohere-transcribe-03-2026",
        quant: "Q4_K_M",
        upstream_repo: "CohereLabs/cohere-transcribe-03-2026",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/cohere-transcribe-03-2026-gguf",
        revision: "ab667acedcb5d8d56837ef3ed3568c3ad50dde47",
        filename: "cohere-transcribe-03-2026-Q4_K_M.gguf",
        size_bytes: 1558162944,
        sha256: "0ea56826d8bd5d74b7143a4a04e022dc1bb75452cfae49d98b6acb0c1d16a1fb",
    },
    CatalogModel {
        id: "cohere-transcribe-03-2026-q5_k_m",
        variant: "cohere-transcribe-03-2026",
        display_name: "cohere-transcribe-03-2026",
        quant: "Q5_K_M",
        upstream_repo: "CohereLabs/cohere-transcribe-03-2026",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/cohere-transcribe-03-2026-gguf",
        revision: "ab667acedcb5d8d56837ef3ed3568c3ad50dde47",
        filename: "cohere-transcribe-03-2026-Q5_K_M.gguf",
        size_bytes: 1770270208,
        sha256: "14d02f1ad6dd77b3a60f82639879012c3adb4fe25c50a5a47a2c4c661daf1558",
    },
    CatalogModel {
        id: "cohere-transcribe-03-2026-q6_k",
        variant: "cohere-transcribe-03-2026",
        display_name: "cohere-transcribe-03-2026",
        quant: "Q6_K",
        upstream_repo: "CohereLabs/cohere-transcribe-03-2026",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/cohere-transcribe-03-2026-gguf",
        revision: "ab667acedcb5d8d56837ef3ed3568c3ad50dde47",
        filename: "cohere-transcribe-03-2026-Q6_K.gguf",
        size_bytes: 1972524544,
        sha256: "094fb65b9d82d4a52db1d944152e4d7b6629f356bb3beaffdd0891eb527f24f3",
    },
    CatalogModel {
        id: "cohere-transcribe-03-2026-q8_0",
        variant: "cohere-transcribe-03-2026",
        display_name: "cohere-transcribe-03-2026",
        quant: "Q8_0",
        upstream_repo: "CohereLabs/cohere-transcribe-03-2026",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/cohere-transcribe-03-2026-gguf",
        revision: "ab667acedcb5d8d56837ef3ed3568c3ad50dde47",
        filename: "cohere-transcribe-03-2026-Q8_0.gguf",
        size_bytes: 2410655232,
        sha256: "931916663432fd895423a4291a8400221802b288967ca2d435fc5e3141c9e71e",
    },
    CatalogModel {
        id: "moonshine-base-f16",
        variant: "moonshine-base",
        display_name: "moonshine-base",
        quant: "F16",
        upstream_repo: "UsefulSensors/moonshine-base",
        license_spdx: "mit",
        hf_repo: "handy-computer/moonshine-base-gguf",
        revision: "cb703342553d960a95e2b4b0f48b8142648ddb22",
        filename: "moonshine-base-F16.gguf",
        size_bytes: 131789440,
        sha256: "c7a158ae0ad47c887041372e97c874e98c11eb0d8eff01279723dde65b358702",
    },
    CatalogModel {
        id: "moonshine-base-f32",
        variant: "moonshine-base",
        display_name: "moonshine-base",
        quant: "F32",
        upstream_repo: "UsefulSensors/moonshine-base",
        license_spdx: "mit",
        hf_repo: "handy-computer/moonshine-base-gguf",
        revision: "cb703342553d960a95e2b4b0f48b8142648ddb22",
        filename: "moonshine-base-F32.gguf",
        size_bytes: 247657088,
        sha256: "b19a36db5a91e4f25cc527c5ed87f774fd037aee8ceec774397c19b7198199fb",
    },
    CatalogModel {
        id: "moonshine-base-q8_0",
        variant: "moonshine-base",
        display_name: "moonshine-base",
        quant: "Q8_0",
        upstream_repo: "UsefulSensors/moonshine-base",
        license_spdx: "mit",
        hf_repo: "handy-computer/moonshine-base-gguf",
        revision: "cb703342553d960a95e2b4b0f48b8142648ddb22",
        filename: "moonshine-base-Q8_0.gguf",
        size_bytes: 77476480,
        sha256: "7f0027dfd857d310b63a85ef57cadf183da712cc374f85a648f8bc18aaa2efc8",
    },
    CatalogModel {
        id: "parakeet-tdt-0.6b-v2-f16",
        variant: "parakeet-tdt-0.6b-v2",
        display_name: "parakeet-tdt-0.6b-v2",
        quant: "F16",
        upstream_repo: "nvidia/parakeet-tdt-0.6b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/parakeet-tdt-0.6b-v2-gguf",
        revision: "382796e405a9fe37145938dccbabfb25e93e4286",
        filename: "parakeet-tdt-0.6b-v2-F16.gguf",
        size_bytes: 1237334592,
        sha256: "8440125213cb92625e3993ec751c3919dd329da9ae9149951cd6f95dd8bafe81",
    },
    CatalogModel {
        id: "parakeet-tdt-0.6b-v2-f32",
        variant: "parakeet-tdt-0.6b-v2",
        display_name: "parakeet-tdt-0.6b-v2",
        quant: "F32",
        upstream_repo: "nvidia/parakeet-tdt-0.6b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/parakeet-tdt-0.6b-v2-gguf",
        revision: "382796e405a9fe37145938dccbabfb25e93e4286",
        filename: "parakeet-tdt-0.6b-v2-F32.gguf",
        size_bytes: 2471550272,
        sha256: "501ba1e61965cdb28689614c648d2c560d75895b47a2948b9010b4e8c9cf1325",
    },
    CatalogModel {
        id: "parakeet-tdt-0.6b-v2-q4_k_m",
        variant: "parakeet-tdt-0.6b-v2",
        display_name: "parakeet-tdt-0.6b-v2",
        quant: "Q4_K_M",
        upstream_repo: "nvidia/parakeet-tdt-0.6b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/parakeet-tdt-0.6b-v2-gguf",
        revision: "382796e405a9fe37145938dccbabfb25e93e4286",
        filename: "parakeet-tdt-0.6b-v2-Q4_K_M.gguf",
        size_bytes: 475491840,
        sha256: "4853f9653f641d376e6f7de65d73c7a34a73677704a606727bf51acc83f999f3",
    },
    CatalogModel {
        id: "parakeet-tdt-0.6b-v2-q5_k_m",
        variant: "parakeet-tdt-0.6b-v2",
        display_name: "parakeet-tdt-0.6b-v2",
        quant: "Q5_K_M",
        upstream_repo: "nvidia/parakeet-tdt-0.6b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/parakeet-tdt-0.6b-v2-gguf",
        revision: "382796e405a9fe37145938dccbabfb25e93e4286",
        filename: "parakeet-tdt-0.6b-v2-Q5_K_M.gguf",
        size_bytes: 539012608,
        sha256: "dfa904520b95451599683613fea47766c378c5fdf5d8cddf48151226b6eaec85",
    },
    CatalogModel {
        id: "parakeet-tdt-0.6b-v2-q6_k",
        variant: "parakeet-tdt-0.6b-v2",
        display_name: "parakeet-tdt-0.6b-v2",
        quant: "Q6_K",
        upstream_repo: "nvidia/parakeet-tdt-0.6b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/parakeet-tdt-0.6b-v2-gguf",
        revision: "382796e405a9fe37145938dccbabfb25e93e4286",
        filename: "parakeet-tdt-0.6b-v2-Q6_K.gguf",
        size_bytes: 600408576,
        sha256: "44c7c363438e03b613cf8ced8b7e6fd1beb2b3805008136fe5e922ab3c79eea9",
    },
    CatalogModel {
        id: "parakeet-tdt-0.6b-v2-q8_0",
        variant: "parakeet-tdt-0.6b-v2",
        display_name: "parakeet-tdt-0.6b-v2",
        quant: "Q8_0",
        upstream_repo: "nvidia/parakeet-tdt-0.6b-v2",
        license_spdx: "cc-by-4.0",
        hf_repo: "handy-computer/parakeet-tdt-0.6b-v2-gguf",
        revision: "382796e405a9fe37145938dccbabfb25e93e4286",
        filename: "parakeet-tdt-0.6b-v2-Q8_0.gguf",
        size_bytes: 729574912,
        sha256: "f0d0e99cebb6d3b83f1f7069b82b5d3c2e39a54545b0da039cb4bafd9c4e5caa",
    },
    CatalogModel {
        id: "whisper-base-f16",
        variant: "whisper-base",
        display_name: "whisper-base",
        quant: "F16",
        upstream_repo: "openai/whisper-base",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base-gguf",
        revision: "30c1704968f7540e112bab3ea3cc4b274902c360",
        filename: "whisper-base-F16.gguf",
        size_bytes: 151145760,
        sha256: "38ab6b0ed742e9eded4d5a2ba7fc34d44fc28cdb71d6360f6321d4b306ef8039",
    },
    CatalogModel {
        id: "whisper-base-f32",
        variant: "whisper-base",
        display_name: "whisper-base",
        quant: "F32",
        upstream_repo: "openai/whisper-base",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base-gguf",
        revision: "30c1704968f7540e112bab3ea3cc4b274902c360",
        filename: "whisper-base-F32.gguf",
        size_bytes: 292335904,
        sha256: "ab30724d251b16383fdbcf88698bcca6649eb98c4e102d6a7271a44d608bb4c6",
    },
    CatalogModel {
        id: "whisper-base-q4_k_m",
        variant: "whisper-base",
        display_name: "whisper-base",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-base",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base-gguf",
        revision: "30c1704968f7540e112bab3ea3cc4b274902c360",
        filename: "whisper-base-Q4_K_M.gguf",
        size_bytes: 58870848,
        sha256: "78bc9a8cb7e16a3354b262a4c71c0ed4892f6a66b681068f474dfada0729c0ae",
    },
    CatalogModel {
        id: "whisper-base-q5_k_m",
        variant: "whisper-base",
        display_name: "whisper-base",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-base",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base-gguf",
        revision: "30c1704968f7540e112bab3ea3cc4b274902c360",
        filename: "whisper-base-Q5_K_M.gguf",
        size_bytes: 63786048,
        sha256: "8e0feb7bc35780353cf31821018e601bb7b7cff6c9a0e17ada5a5db23f4db867",
    },
    CatalogModel {
        id: "whisper-base-q6_k",
        variant: "whisper-base",
        display_name: "whisper-base",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-base",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base-gguf",
        revision: "30c1704968f7540e112bab3ea3cc4b274902c360",
        filename: "whisper-base-Q6_K.gguf",
        size_bytes: 67865664,
        sha256: "6e4ac95aef48f28b5dd11b0be26a65e785ffbf3682cb076560a27e72206ea740",
    },
    CatalogModel {
        id: "whisper-base-q8_0",
        variant: "whisper-base",
        display_name: "whisper-base",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-base",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base-gguf",
        revision: "30c1704968f7540e112bab3ea3cc4b274902c360",
        filename: "whisper-base-Q8_0.gguf",
        size_bytes: 84962880,
        sha256: "81c069428bc8a24551a8169cf31cf09bcfd9d4cf50389ae281323c9aa9648c81",
    },
    CatalogModel {
        id: "whisper-base.en-f16",
        variant: "whisper-base.en",
        display_name: "whisper-base.en",
        quant: "F16",
        upstream_repo: "openai/whisper-base.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base.en-gguf",
        revision: "f83d8d187299d4fc06e86a6de5084375ceac34bf",
        filename: "whisper-base.en-F16.gguf",
        size_bytes: 151068608,
        sha256: "e6c447b0cc761e8ac3fc2f5e0e6918f604e415a41cff97825f1503ccde376e1e",
    },
    CatalogModel {
        id: "whisper-base.en-f32",
        variant: "whisper-base.en",
        display_name: "whisper-base.en",
        quant: "F32",
        upstream_repo: "openai/whisper-base.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base.en-gguf",
        revision: "f83d8d187299d4fc06e86a6de5084375ceac34bf",
        filename: "whisper-base.en-F32.gguf",
        size_bytes: 292257728,
        sha256: "f694f59682d44bafe4e74b8b9cc395dfdc3737a4d5e1910e399d45683729ad2d",
    },
    CatalogModel {
        id: "whisper-base.en-q4_k_m",
        variant: "whisper-base.en",
        display_name: "whisper-base.en",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-base.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base.en-gguf",
        revision: "f83d8d187299d4fc06e86a6de5084375ceac34bf",
        filename: "whisper-base.en-Q4_K_M.gguf",
        size_bytes: 58794272,
        sha256: "fd19959ff81f96c2b66f26fad97aada090ed6b3af681cf5b569bef3d7a073da5",
    },
    CatalogModel {
        id: "whisper-base.en-q5_k_m",
        variant: "whisper-base.en",
        display_name: "whisper-base.en",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-base.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base.en-gguf",
        revision: "f83d8d187299d4fc06e86a6de5084375ceac34bf",
        filename: "whisper-base.en-Q5_K_M.gguf",
        size_bytes: 63709472,
        sha256: "549e6e991588cd748dc6bf53b8fe47629dc08ed9038b8955270ee98fbc5e4c54",
    },
    CatalogModel {
        id: "whisper-base.en-q6_k",
        variant: "whisper-base.en",
        display_name: "whisper-base.en",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-base.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base.en-gguf",
        revision: "f83d8d187299d4fc06e86a6de5084375ceac34bf",
        filename: "whisper-base.en-Q6_K.gguf",
        size_bytes: 67789088,
        sha256: "5de8fc00f70f538af2811c9992de502513d6d60336f32ed975b4b6eb91b69e73",
    },
    CatalogModel {
        id: "whisper-base.en-q8_0",
        variant: "whisper-base.en",
        display_name: "whisper-base.en",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-base.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-base.en-gguf",
        revision: "f83d8d187299d4fc06e86a6de5084375ceac34bf",
        filename: "whisper-base.en-Q8_0.gguf",
        size_bytes: 84886208,
        sha256: "3b46ca40bccbf7609c68d88a36d96077a04ca7c87f2060ede06f129fac3e7652",
    },
    CatalogModel {
        id: "whisper-large-f16",
        variant: "whisper-large",
        display_name: "whisper-large",
        quant: "F16",
        upstream_repo: "openai/whisper-large",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-gguf",
        revision: "cb93fc5b8f7f06658687a624af75e9ca54bc8057",
        filename: "whisper-large-F16.gguf",
        size_bytes: 3106458176,
        sha256: "2c6198aeb009e01bf03cce9a30374b86ecd3484cd53b245e4a9f5bf7a670c498",
    },
    CatalogModel {
        id: "whisper-large-f32",
        variant: "whisper-large",
        display_name: "whisper-large",
        quant: "F32",
        upstream_repo: "openai/whisper-large",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-gguf",
        revision: "cb93fc5b8f7f06658687a624af75e9ca54bc8057",
        filename: "whisper-large-F32.gguf",
        size_bytes: 6175245376,
        sha256: "fafb2d4c6f865fc6cd745be8781eaf87d8d7a289cf3556283849fb1cc6741717",
    },
    CatalogModel {
        id: "whisper-large-q4_k_m",
        variant: "whisper-large",
        display_name: "whisper-large",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-large",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-gguf",
        revision: "cb93fc5b8f7f06658687a624af75e9ca54bc8057",
        filename: "whisper-large-Q4_K_M.gguf",
        size_bytes: 996526048,
        sha256: "7d7693afc06096de46f2a41309ac479909e131824e78dae4e205f35463aaec09",
    },
    CatalogModel {
        id: "whisper-large-q5_k_m",
        variant: "whisper-large",
        display_name: "whisper-large",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-large",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-gguf",
        revision: "cb93fc5b8f7f06658687a624af75e9ca54bc8057",
        filename: "whisper-large-Q5_K_M.gguf",
        size_bytes: 1160366048,
        sha256: "b1dfb9fa0e9b0574d8c822f51662b8e59915831e1a3ca0d191f18ff8ba742639",
    },
    CatalogModel {
        id: "whisper-large-q6_k",
        variant: "whisper-large",
        display_name: "whisper-large",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-large",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-gguf",
        revision: "cb93fc5b8f7f06658687a624af75e9ca54bc8057",
        filename: "whisper-large-Q6_K.gguf",
        size_bytes: 1296353248,
        sha256: "04756b664385d0ec26af1ef43b94bccdaf2d6dce83b52f482d2232b67642b6bc",
    },
    CatalogModel {
        id: "whisper-large-q8_0",
        variant: "whisper-large",
        display_name: "whisper-large",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-large",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-gguf",
        revision: "cb93fc5b8f7f06658687a624af75e9ca54bc8057",
        filename: "whisper-large-Q8_0.gguf",
        size_bytes: 1667964192,
        sha256: "0b9c63f73c68327d6ba2386e40c4a4dc46a9e7114a78c93a464b48b72c001547",
    },
    CatalogModel {
        id: "whisper-large-v2-f16",
        variant: "whisper-large-v2",
        display_name: "whisper-large-v2",
        quant: "F16",
        upstream_repo: "openai/whisper-large-v2",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v2-gguf",
        revision: "25062d25fc998138a697514d047c30f5a5faa410",
        filename: "whisper-large-v2-F16.gguf",
        size_bytes: 3106458208,
        sha256: "c8ffa39202367b896cdf642cd28c28fb9ac4d90f7f12795a219952c70bc63f04",
    },
    CatalogModel {
        id: "whisper-large-v2-f32",
        variant: "whisper-large-v2",
        display_name: "whisper-large-v2",
        quant: "F32",
        upstream_repo: "openai/whisper-large-v2",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v2-gguf",
        revision: "25062d25fc998138a697514d047c30f5a5faa410",
        filename: "whisper-large-v2-F32.gguf",
        size_bytes: 6175245408,
        sha256: "eed42074ca931551b6bb9c80e658bb0d646c80da06acb03a49cb6326d2a57ced",
    },
    CatalogModel {
        id: "whisper-large-v2-q4_k_m",
        variant: "whisper-large-v2",
        display_name: "whisper-large-v2",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-large-v2",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v2-gguf",
        revision: "25062d25fc998138a697514d047c30f5a5faa410",
        filename: "whisper-large-v2-Q4_K_M.gguf",
        size_bytes: 996526080,
        sha256: "76aa37b205abc1fb7a9e7aaf0655b8747995b81e6bb72c18f4b1acf59e222f79",
    },
    CatalogModel {
        id: "whisper-large-v2-q5_k_m",
        variant: "whisper-large-v2",
        display_name: "whisper-large-v2",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-large-v2",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v2-gguf",
        revision: "25062d25fc998138a697514d047c30f5a5faa410",
        filename: "whisper-large-v2-Q5_K_M.gguf",
        size_bytes: 1160366080,
        sha256: "94fab1289a98d26bf375174a8daa00f44c7b16a9944726dde05361e5a8bd07ff",
    },
    CatalogModel {
        id: "whisper-large-v2-q6_k",
        variant: "whisper-large-v2",
        display_name: "whisper-large-v2",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-large-v2",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v2-gguf",
        revision: "25062d25fc998138a697514d047c30f5a5faa410",
        filename: "whisper-large-v2-Q6_K.gguf",
        size_bytes: 1296353280,
        sha256: "d216cb645a4a0b158a465f55c18a8317872788c38b3dbc0895c00d5c3bba72c8",
    },
    CatalogModel {
        id: "whisper-large-v2-q8_0",
        variant: "whisper-large-v2",
        display_name: "whisper-large-v2",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-large-v2",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v2-gguf",
        revision: "25062d25fc998138a697514d047c30f5a5faa410",
        filename: "whisper-large-v2-Q8_0.gguf",
        size_bytes: 1667964224,
        sha256: "704603cff0f82ddd7f7f51045e12e5c8f85a37a9edec72ab10fd8ede32220702",
    },
    CatalogModel {
        id: "whisper-large-v3-f16",
        variant: "whisper-large-v3",
        display_name: "whisper-large-v3",
        quant: "F16",
        upstream_repo: "openai/whisper-large-v3",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-gguf",
        revision: "b33a05f1459f33b0c876f014a57d51618b77d754",
        filename: "whisper-large-v3-F16.gguf",
        size_bytes: 3107236640,
        sha256: "e633ab1d74b0e98f4f57daedaee34291297dbbf01389bda3d890766600c1c584",
    },
    CatalogModel {
        id: "whisper-large-v3-q4_k_m",
        variant: "whisper-large-v3",
        display_name: "whisper-large-v3",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-large-v3",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-gguf",
        revision: "b33a05f1459f33b0c876f014a57d51618b77d754",
        filename: "whisper-large-v3-Q4_K_M.gguf",
        size_bytes: 997303008,
        sha256: "6fe933811cec4cd3159debc46520ecd3aac6c7e322ece2ac61fcfdab184e1fe0",
    },
    CatalogModel {
        id: "whisper-large-v3-q5_k_m",
        variant: "whisper-large-v3",
        display_name: "whisper-large-v3",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-large-v3",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-gguf",
        revision: "b33a05f1459f33b0c876f014a57d51618b77d754",
        filename: "whisper-large-v3-Q5_K_M.gguf",
        size_bytes: 1161143008,
        sha256: "6053d0fd69a0fd48b8fea5ea7a52b9e0cde389343566fa30e453a1b2b258dc38",
    },
    CatalogModel {
        id: "whisper-large-v3-q6_k",
        variant: "whisper-large-v3",
        display_name: "whisper-large-v3",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-large-v3",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-gguf",
        revision: "b33a05f1459f33b0c876f014a57d51618b77d754",
        filename: "whisper-large-v3-Q6_K.gguf",
        size_bytes: 1297130208,
        sha256: "88e6dc60bd0538da145a50c19f4778e4a106fe28713adeeca43163215ee02805",
    },
    CatalogModel {
        id: "whisper-large-v3-q8_0",
        variant: "whisper-large-v3",
        display_name: "whisper-large-v3",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-large-v3",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-gguf",
        revision: "b33a05f1459f33b0c876f014a57d51618b77d754",
        filename: "whisper-large-v3-Q8_0.gguf",
        size_bytes: 1668741440,
        sha256: "2fa1a5f179f8a511a53e2108db270aa4af3ce08cd976af4180e2854666bb4ba3",
    },
    CatalogModel {
        id: "whisper-large-v3-turbo-f16",
        variant: "whisper-large-v3-turbo",
        display_name: "Whisper large-v3-turbo",
        quant: "F16",
        upstream_repo: "openai/whisper-large-v3-turbo",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-turbo-gguf",
        revision: "ceea6c8a94a21ab85be244d311e874a39344dbf5",
        filename: "whisper-large-v3-turbo-F16.gguf",
        size_bytes: 1625935520,
        sha256: "e1d0144e9afc9f479d9e51fc92c7dea9dc36059655eeb3819f16ad2de779046a",
    },
    CatalogModel {
        id: "whisper-large-v3-turbo-q4_k_m",
        variant: "whisper-large-v3-turbo",
        display_name: "Whisper large-v3-turbo",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-large-v3-turbo",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-turbo-gguf",
        revision: "ceea6c8a94a21ab85be244d311e874a39344dbf5",
        filename: "whisper-large-v3-turbo-Q4_K_M.gguf",
        size_bytes: 536069728,
        sha256: "ecfe9b6beb4ab18fef49187cc968cc74b5168b94629c8830e2ca6b794c6e25ed",
    },
    CatalogModel {
        id: "whisper-large-v3-turbo-q5_k_m",
        variant: "whisper-large-v3-turbo",
        display_name: "Whisper large-v3-turbo",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-large-v3-turbo",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-turbo-gguf",
        revision: "ceea6c8a94a21ab85be244d311e874a39344dbf5",
        filename: "whisper-large-v3-turbo-Q5_K_M.gguf",
        size_bytes: 619628128,
        sha256: "977b5db4e004349dffd1ab9caa10ba5aaba3fc3edd3ba72cadb84328a3203e36",
    },
    CatalogModel {
        id: "whisper-large-v3-turbo-q6_k",
        variant: "whisper-large-v3-turbo",
        display_name: "Whisper large-v3-turbo",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-large-v3-turbo",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-turbo-gguf",
        revision: "ceea6c8a94a21ab85be244d311e874a39344dbf5",
        filename: "whisper-large-v3-turbo-Q6_K.gguf",
        size_bytes: 692536928,
        sha256: "3881dc17bf345f7ce4a3c48327074951e1550c8a1e89fa58d554d9f26a1b6274",
    },
    CatalogModel {
        id: "whisper-large-v3-turbo-q8_0",
        variant: "whisper-large-v3-turbo",
        display_name: "Whisper large-v3-turbo",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-large-v3-turbo",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-large-v3-turbo-gguf",
        revision: "ceea6c8a94a21ab85be244d311e874a39344dbf5",
        filename: "whisper-large-v3-turbo-Q8_0.gguf",
        size_bytes: 886381760,
        sha256: "b2e30cc286bc9f3aba4db9099fc7403543497c05ce7100d0d83091ddfd25a183",
    },
    CatalogModel {
        id: "whisper-medium-f16",
        variant: "whisper-medium",
        display_name: "whisper-medium",
        quant: "F16",
        upstream_repo: "openai/whisper-medium",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium-gguf",
        revision: "835ad19fff976143d650e35113a6544980cbd983",
        filename: "whisper-medium-F16.gguf",
        size_bytes: 1541931424,
        sha256: "62338e5194cb9ccc6734adf6f42694805a98a158028b2022e39ec060559bd517",
    },
    CatalogModel {
        id: "whisper-medium-f32",
        variant: "whisper-medium",
        display_name: "whisper-medium",
        quant: "F32",
        upstream_repo: "openai/whisper-medium",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium-gguf",
        revision: "835ad19fff976143d650e35113a6544980cbd983",
        filename: "whisper-medium-F32.gguf",
        size_bytes: 3057437088,
        sha256: "d716aa436e8d1d3164898d1fd6f4bde0f7cbbb7059b8c276b18f78cbca2faf6b",
    },
    CatalogModel {
        id: "whisper-medium-q4_k_m",
        variant: "whisper-medium",
        display_name: "whisper-medium",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-medium",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium-gguf",
        revision: "835ad19fff976143d650e35113a6544980cbd983",
        filename: "whisper-medium-Q4_K_M.gguf",
        size_bytes: 504102848,
        sha256: "6c9cad9c41d1fa0fcc624898f7ee3485b66127627878081a5655c6f6f3ce20ab",
    },
    CatalogModel {
        id: "whisper-medium-q5_k_m",
        variant: "whisper-medium",
        display_name: "whisper-medium",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-medium",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium-gguf",
        revision: "835ad19fff976143d650e35113a6544980cbd983",
        filename: "whisper-medium-Q5_K_M.gguf",
        size_bytes: 582746048,
        sha256: "4e2a8904a866b3aa7ef70d7640ec6abc5f0a05524cd950ea4b66ace12122bf53",
    },
    CatalogModel {
        id: "whisper-medium-q6_k",
        variant: "whisper-medium",
        display_name: "whisper-medium",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-medium",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium-gguf",
        revision: "835ad19fff976143d650e35113a6544980cbd983",
        filename: "whisper-medium-Q6_K.gguf",
        size_bytes: 648019904,
        sha256: "8718bafef4f595b5cacd9d9409fb7e19ce99b1afe4c6a450198d692fa58bd380",
    },
    CatalogModel {
        id: "whisper-medium-q8_0",
        variant: "whisper-medium",
        display_name: "whisper-medium",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-medium",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium-gguf",
        revision: "835ad19fff976143d650e35113a6544980cbd983",
        filename: "whisper-medium-Q8_0.gguf",
        size_bytes: 831538144,
        sha256: "09e6a65e7de377aa5b10bae24608bc6f8ca2ed04b3993ef10d4a02bcd9a82adf",
    },
    CatalogModel {
        id: "whisper-medium.en-f16",
        variant: "whisper-medium.en",
        display_name: "whisper-medium.en",
        quant: "F16",
        upstream_repo: "openai/whisper-medium.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium.en-gguf",
        revision: "f467a42fbc6158c3f6943280b2d243e528658b9a",
        filename: "whisper-medium.en-F16.gguf",
        size_bytes: 1541853248,
        sha256: "c8be41b68306ad35382af749d4681b33da403b7d82eb3b6d3225a01a02dc1196",
    },
    CatalogModel {
        id: "whisper-medium.en-f32",
        variant: "whisper-medium.en",
        display_name: "whisper-medium.en",
        quant: "F32",
        upstream_repo: "openai/whisper-medium.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium.en-gguf",
        revision: "f467a42fbc6158c3f6943280b2d243e528658b9a",
        filename: "whisper-medium.en-F32.gguf",
        size_bytes: 3057356864,
        sha256: "6a0bcb8c3360dbc8101a1ebcf2776d1e3463538c4b9486da97b8f21e462e6039",
    },
    CatalogModel {
        id: "whisper-medium.en-q4_k_m",
        variant: "whisper-medium.en",
        display_name: "whisper-medium.en",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-medium.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium.en-gguf",
        revision: "f467a42fbc6158c3f6943280b2d243e528658b9a",
        filename: "whisper-medium.en-Q4_K_M.gguf",
        size_bytes: 504025856,
        sha256: "8a0eb0bb20019af88fc0ccda06d97dc949a6a0cdf35d3f2c316b967e76cd4077",
    },
    CatalogModel {
        id: "whisper-medium.en-q5_k_m",
        variant: "whisper-medium.en",
        display_name: "whisper-medium.en",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-medium.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium.en-gguf",
        revision: "f467a42fbc6158c3f6943280b2d243e528658b9a",
        filename: "whisper-medium.en-Q5_K_M.gguf",
        size_bytes: 582669056,
        sha256: "8c90b55725036f1362fc5a583432a4e63370304dbf0f96b6245685f8a588174f",
    },
    CatalogModel {
        id: "whisper-medium.en-q6_k",
        variant: "whisper-medium.en",
        display_name: "whisper-medium.en",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-medium.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium.en-gguf",
        revision: "f467a42fbc6158c3f6943280b2d243e528658b9a",
        filename: "whisper-medium.en-Q6_K.gguf",
        size_bytes: 647942912,
        sha256: "9abdf673a4096ccb8bd210a3c68d73665c3ac8450bdeb54fd35c255a28b63d55",
    },
    CatalogModel {
        id: "whisper-medium.en-q8_0",
        variant: "whisper-medium.en",
        display_name: "whisper-medium.en",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-medium.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-medium.en-gguf",
        revision: "f467a42fbc6158c3f6943280b2d243e528658b9a",
        filename: "whisper-medium.en-Q8_0.gguf",
        size_bytes: 831460928,
        sha256: "03d7257fef498750ce272631bc6a34de322fc2b438aab5c268ff49dfd1b64c49",
    },
    CatalogModel {
        id: "whisper-small-f16",
        variant: "whisper-small",
        display_name: "whisper-small",
        quant: "F16",
        upstream_repo: "openai/whisper-small",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small-gguf",
        revision: "a2073177cb69bd74b9ca9460b852d17fbfd5d68c",
        filename: "whisper-small-F16.gguf",
        size_bytes: 492888480,
        sha256: "bef65e1ac9d012269453243243aac0d0f67792693ba6c99b584a124e0ee326fc",
    },
    CatalogModel {
        id: "whisper-small-f32",
        variant: "whisper-small",
        display_name: "whisper-small",
        quant: "F32",
        upstream_repo: "openai/whisper-small",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small-gguf",
        revision: "a2073177cb69bd74b9ca9460b852d17fbfd5d68c",
        filename: "whisper-small-F32.gguf",
        size_bytes: 968914848,
        sha256: "6bc797078fdddcc890999130dfc33dfc30d82de5a9115a840ec73853262a9d21",
    },
    CatalogModel {
        id: "whisper-small-q4_k_m",
        variant: "whisper-small",
        display_name: "whisper-small",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-small",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small-gguf",
        revision: "a2073177cb69bd74b9ca9460b852d17fbfd5d68c",
        filename: "whisper-small-Q4_K_M.gguf",
        size_bytes: 171630656,
        sha256: "b204d2005a3e5d4fe6153bd61e5e8b32e757ff7b017ac8f61c6f051c2f80e939",
    },
    CatalogModel {
        id: "whisper-small-q5_k_m",
        variant: "whisper-small",
        display_name: "whisper-small",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-small",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small-gguf",
        revision: "a2073177cb69bd74b9ca9460b852d17fbfd5d68c",
        filename: "whisper-small-Q5_K_M.gguf",
        size_bytes: 193749056,
        sha256: "326cd00c3e7217c751667c7c1600eaf7e0de174e186ca2c16b4bf590251c3c3b",
    },
    CatalogModel {
        id: "whisper-small-q6_k",
        variant: "whisper-small",
        display_name: "whisper-small",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-small",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small-gguf",
        revision: "a2073177cb69bd74b9ca9460b852d17fbfd5d68c",
        filename: "whisper-small-Q6_K.gguf",
        size_bytes: 212107328,
        sha256: "4d14215a972101fff787fc59e30b908ee511c725600317e02c9032cb8799f32d",
    },
    CatalogModel {
        id: "whisper-small-q8_0",
        variant: "whisper-small",
        display_name: "whisper-small",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-small",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small-gguf",
        revision: "a2073177cb69bd74b9ca9460b852d17fbfd5d68c",
        filename: "whisper-small-Q8_0.gguf",
        size_bytes: 269751136,
        sha256: "9b9c8811bbcc82a7766f0fb0925614bdacb0923b2cc630daeac17108b655b860",
    },
    CatalogModel {
        id: "whisper-small.en-f16",
        variant: "whisper-small.en",
        display_name: "whisper-small.en",
        quant: "F16",
        upstream_repo: "openai/whisper-small.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small.en-gguf",
        revision: "465682f43898836b623801603ca6432a41275586",
        filename: "whisper-small.en-F16.gguf",
        size_bytes: 492810784,
        sha256: "59eade60e072bd289c9ff39313ef14b565691bfe373275ea904ecea497e47c8a",
    },
    CatalogModel {
        id: "whisper-small.en-f32",
        variant: "whisper-small.en",
        display_name: "whisper-small.en",
        quant: "F32",
        upstream_repo: "openai/whisper-small.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small.en-gguf",
        revision: "465682f43898836b623801603ca6432a41275586",
        filename: "whisper-small.en-F32.gguf",
        size_bytes: 968835616,
        sha256: "fce06c84aa930d0652d292781b4470fb4d5631527e211fd4f402703f63ee7291",
    },
    CatalogModel {
        id: "whisper-small.en-q4_k_m",
        variant: "whisper-small.en",
        display_name: "whisper-small.en",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-small.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small.en-gguf",
        revision: "465682f43898836b623801603ca6432a41275586",
        filename: "whisper-small.en-Q4_K_M.gguf",
        size_bytes: 171553856,
        sha256: "7bf9ebce1e6332b3d18d4c27032e3097148fc124befed3e70022e0d70539a365",
    },
    CatalogModel {
        id: "whisper-small.en-q5_k_m",
        variant: "whisper-small.en",
        display_name: "whisper-small.en",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-small.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small.en-gguf",
        revision: "465682f43898836b623801603ca6432a41275586",
        filename: "whisper-small.en-Q5_K_M.gguf",
        size_bytes: 193672256,
        sha256: "e96a679e57d36f80bb26512f3893185d219ddd8880cdac4c433e5af13a001b47",
    },
    CatalogModel {
        id: "whisper-small.en-q6_k",
        variant: "whisper-small.en",
        display_name: "whisper-small.en",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-small.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small.en-gguf",
        revision: "465682f43898836b623801603ca6432a41275586",
        filename: "whisper-small.en-Q6_K.gguf",
        size_bytes: 212030528,
        sha256: "3703d6f9e4d78b5b2f196f9c3cfab5c1b6b268fe42fff311cb89137506f92019",
    },
    CatalogModel {
        id: "whisper-small.en-q8_0",
        variant: "whisper-small.en",
        display_name: "whisper-small.en",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-small.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-small.en-gguf",
        revision: "465682f43898836b623801603ca6432a41275586",
        filename: "whisper-small.en-Q8_0.gguf",
        size_bytes: 269674144,
        sha256: "9614e6b7fda2d26018e4f268aece8ca25a83296ea0b534169a585b740bfd71ef",
    },
    CatalogModel {
        id: "whisper-tiny-f16",
        variant: "whisper-tiny",
        display_name: "whisper-tiny",
        quant: "F16",
        upstream_repo: "openai/whisper-tiny",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny-gguf",
        revision: "2678cc66038359b97c8e6fd6454c56fc9006d571",
        filename: "whisper-tiny-F16.gguf",
        size_bytes: 80135360,
        sha256: "5b44043278b47d3b6e56fb16c6bc5bb0aa16f2e69086f4d67175ed0a30d6a987",
    },
    CatalogModel {
        id: "whisper-tiny-f32",
        variant: "whisper-tiny",
        display_name: "whisper-tiny",
        quant: "F32",
        upstream_repo: "openai/whisper-tiny",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny-gguf",
        revision: "2678cc66038359b97c8e6fd6454c56fc9006d571",
        filename: "whisper-tiny-F32.gguf",
        size_bytes: 152997824,
        sha256: "f5e0ecdd9967173fdede25ceb7e5159b3294f05b8dc0c87b0876fccd36f18982",
    },
    CatalogModel {
        id: "whisper-tiny-q4_k_m",
        variant: "whisper-tiny",
        display_name: "whisper-tiny",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-tiny",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny-gguf",
        revision: "2678cc66038359b97c8e6fd6454c56fc9006d571",
        filename: "whisper-tiny-Q4_K_M.gguf",
        size_bytes: 43621792,
        sha256: "86e1100470df173fa9079f941e33e4d52d11c03923407dd1ec51fd1937d8e609",
    },
    CatalogModel {
        id: "whisper-tiny-q5_k_m",
        variant: "whisper-tiny",
        display_name: "whisper-tiny",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-tiny",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny-gguf",
        revision: "2678cc66038359b97c8e6fd6454c56fc9006d571",
        filename: "whisper-tiny-Q5_K_M.gguf",
        size_bytes: 44211616,
        sha256: "72cfa8ee436a635a5b6fb373cc056a828b9efe96d32d6eb8769ed3cc5b429719",
    },
    CatalogModel {
        id: "whisper-tiny-q6_k",
        variant: "whisper-tiny",
        display_name: "whisper-tiny",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-tiny",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny-gguf",
        revision: "2678cc66038359b97c8e6fd6454c56fc9006d571",
        filename: "whisper-tiny-Q6_K.gguf",
        size_bytes: 44838304,
        sha256: "e23e79dbc42e90171e050b0e7da00f07e41343547ef09d86588324b3c5672413",
    },
    CatalogModel {
        id: "whisper-tiny-q8_0",
        variant: "whisper-tiny",
        display_name: "whisper-tiny",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-tiny",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny-gguf",
        revision: "2678cc66038359b97c8e6fd6454c56fc9006d571",
        filename: "whisper-tiny-Q8_0.gguf",
        size_bytes: 45981088,
        sha256: "325b9c7997cd1eff81ef709d55766565e71be696130cc3a3d444713798706834",
    },
    CatalogModel {
        id: "whisper-tiny.en-f16",
        variant: "whisper-tiny.en",
        display_name: "whisper-tiny.en",
        quant: "F16",
        upstream_repo: "openai/whisper-tiny.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny.en-gguf",
        revision: "f2406b60206a289ed322f1cf8c492c070c042b1f",
        filename: "whisper-tiny.en-F16.gguf",
        size_bytes: 80058464,
        sha256: "7ae6eae54d625472e52b6af03b09dec586f0e948ffa5872815bb1988e973ad2b",
    },
    CatalogModel {
        id: "whisper-tiny.en-f32",
        variant: "whisper-tiny.en",
        display_name: "whisper-tiny.en",
        quant: "F32",
        upstream_repo: "openai/whisper-tiny.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny.en-gguf",
        revision: "f2406b60206a289ed322f1cf8c492c070c042b1f",
        filename: "whisper-tiny.en-F32.gguf",
        size_bytes: 152920160,
        sha256: "4aaf936f214a08f199ff7daecf421eb28be273e4e4b3bba0486b98efed140045",
    },
    CatalogModel {
        id: "whisper-tiny.en-q4_k_m",
        variant: "whisper-tiny.en",
        display_name: "whisper-tiny.en",
        quant: "Q4_K_M",
        upstream_repo: "openai/whisper-tiny.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny.en-gguf",
        revision: "f2406b60206a289ed322f1cf8c492c070c042b1f",
        filename: "whisper-tiny.en-Q4_K_M.gguf",
        size_bytes: 43545248,
        sha256: "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b",
    },
    CatalogModel {
        id: "whisper-tiny.en-q5_k_m",
        variant: "whisper-tiny.en",
        display_name: "whisper-tiny.en",
        quant: "Q5_K_M",
        upstream_repo: "openai/whisper-tiny.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny.en-gguf",
        revision: "f2406b60206a289ed322f1cf8c492c070c042b1f",
        filename: "whisper-tiny.en-Q5_K_M.gguf",
        size_bytes: 44135072,
        sha256: "610a75700023ffd74df1ab538353525d0a82a3e35702b923e955e69422519740",
    },
    CatalogModel {
        id: "whisper-tiny.en-q6_k",
        variant: "whisper-tiny.en",
        display_name: "whisper-tiny.en",
        quant: "Q6_K",
        upstream_repo: "openai/whisper-tiny.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny.en-gguf",
        revision: "f2406b60206a289ed322f1cf8c492c070c042b1f",
        filename: "whisper-tiny.en-Q6_K.gguf",
        size_bytes: 44761760,
        sha256: "33b349eabeb0808562060b44dd539264f94365f09ed5317337207b955355abe4",
    },
    CatalogModel {
        id: "whisper-tiny.en-q8_0",
        variant: "whisper-tiny.en",
        display_name: "whisper-tiny.en",
        quant: "Q8_0",
        upstream_repo: "openai/whisper-tiny.en",
        license_spdx: "apache-2.0",
        hf_repo: "handy-computer/whisper-tiny.en-gguf",
        revision: "f2406b60206a289ed322f1cf8c492c070c042b1f",
        filename: "whisper-tiny.en-Q8_0.gguf",
        size_bytes: 45904544,
        sha256: "e8c9b73c06344307d8b346e07fbe93dd88d894627854bcff31523f1ce44394fa",
    },
];

/// A single pinned, integrity-checked download outside the speech-model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedFile {
    pub url: &'static str,
    pub filename: &'static str,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the file contents at `url`.
    pub sha256: &'static str,
}

/// Silero VAD v4, pinned to a Handy commit and verified like catalog entries.
pub const VAD_MODEL: PinnedFile = PinnedFile {
    url: "https://raw.githubusercontent.com/cjpais/Handy/8f9cf53cd1410cda26beea39ff802ac306e39585/src-tauri/resources/models/silero_vad_v4.onnx",
    filename: "silero_vad_v4.onnx",
    size_bytes: 1807522,
    sha256: "a35ebf52fd3ce5f1469b2a36158dba761bc47b973ea3382b3186ca15b1f5af28",
};

/// The default active model for every backend (CPU and every accelerator
/// alike): the one GGUF file `InferenceEngine::load` tries first on an
/// accelerator and falls back to on CPU. Matches the GPU path's historical
/// hardcoded default (same repo, revision, filename, and SHA-256 already
/// verified in the slice 2 GPU benchmark report), so existing GPU-build
/// installs are unaffected, and is also the nearest GGUF equivalent (same
/// `small` family, a comparable quantization) to the legacy CPU-only build's
/// `whisper-small-q5_1` default that `config::migrate_legacy_model_config`
/// maps pre-consolidation configs onto.
pub const DEFAULT_MODEL_ID: &str = "whisper-small-q5_k_m";

const ENGLISH_LANGUAGES: &[&str] = &["en"];
const BREEZE_LANGUAGES: &[&str] = &["zh", "en"];

const WHISPER_MULTILINGUAL: &[&str] = &[
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "cs", "cy", "da",
    "de", "el", "en", "es", "et", "eu", "fa", "fi", "fo", "fr", "gl", "gu", "haw", "ha", "he",
    "hi", "hr", "ht", "hu", "hy", "id", "is", "it", "ja", "jw", "ka", "kk", "km", "kn", "ko", "la",
    "lb", "ln", "lo", "lt", "lv", "mg", "mi", "mk", "ml", "mn", "mr", "ms", "mt", "my", "ne", "nl",
    "nn", "no", "oc", "pa", "pl", "ps", "pt", "ro", "ru", "sa", "sd", "si", "sk", "sl", "sn", "so",
    "sq", "sr", "su", "sv", "sw", "ta", "te", "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz",
    "vi", "yi", "yo", "zh",
];

const WHISPER_MULTILINGUAL_V3: &[&str] = &[
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "cs", "cy", "da",
    "de", "el", "en", "es", "et", "eu", "fa", "fi", "fo", "fr", "gl", "gu", "haw", "ha", "he",
    "hi", "hr", "ht", "hu", "hy", "id", "is", "it", "ja", "jw", "ka", "kk", "km", "kn", "ko", "la",
    "lb", "ln", "lo", "lt", "lv", "mg", "mi", "mk", "ml", "mn", "mr", "ms", "mt", "my", "ne", "nl",
    "nn", "no", "oc", "pa", "pl", "ps", "pt", "ro", "ru", "sa", "sd", "si", "sk", "sl", "sn", "so",
    "sq", "sr", "su", "sv", "sw", "ta", "te", "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz",
    "vi", "yi", "yo", "yue", "zh",
];

const CANARY_LANGUAGES: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
];

const COHERE_LANGUAGES: &[&str] = &[
    "en", "fr", "de", "es", "it", "pt", "nl", "pl", "el", "ar", "ja", "zh", "vi", "ko",
];

/// Capability data for every transcribe.cpp variant the catalog draws from,
/// so the UI can label each entry with its family and language coverage and
/// config validation can read each family's single-buffer limit. The four
/// non-whisper families were admitted after checking their offline output and
/// long-form behavior against this project's pipeline (see the module docs).
pub const VARIANTS: &[VariantInfo] = &[
    VariantInfo {
        variant: "breeze-asr-25",
        family: "whisper",
        languages: BREEZE_LANGUAGES,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "canary-1b-v2",
        family: "canary",
        languages: CANARY_LANGUAGES,
        long_form: LongForm::HardCap { cap_seconds: 400 },
    },
    VariantInfo {
        variant: "cohere-transcribe-03-2026",
        family: "cohere_asr",
        languages: COHERE_LANGUAGES,
        long_form: LongForm::HardCap { cap_seconds: 400 },
    },
    VariantInfo {
        variant: "moonshine-base",
        family: "moonshine",
        languages: ENGLISH_LANGUAGES,
        long_form: LongForm::SoftWindow { window_seconds: 48 },
    },
    VariantInfo {
        variant: "parakeet-tdt-0.6b-v2",
        family: "parakeet",
        languages: ENGLISH_LANGUAGES,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-base",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-base.en",
        family: "whisper",
        languages: ENGLISH_LANGUAGES,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-large",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-large-v2",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-large-v3",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL_V3,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-large-v3-turbo",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL_V3,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-medium",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-medium.en",
        family: "whisper",
        languages: ENGLISH_LANGUAGES,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-small",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-small.en",
        family: "whisper",
        languages: ENGLISH_LANGUAGES,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-tiny",
        family: "whisper",
        languages: WHISPER_MULTILINGUAL,
        long_form: LongForm::Unbounded,
    },
    VariantInfo {
        variant: "whisper-tiny.en",
        family: "whisper",
        languages: ENGLISH_LANGUAGES,
        long_form: LongForm::Unbounded,
    },
];

/// Capability data for a transcribe.cpp `variant` slug.
pub fn variant_info(variant: &str) -> Option<&'static VariantInfo> {
    VARIANTS.iter().find(|info| info.variant == variant)
}

pub fn find(id: &str) -> Option<&'static CatalogModel> {
    ENTRIES.iter().find(|entry| entry.id == id)
}

/// Local path a catalog model is (or would be) installed at.
pub fn model_path(id: &str) -> anyhow::Result<std::path::PathBuf> {
    let entry = find(id).ok_or_else(|| anyhow::anyhow!("unknown catalog model: {id}"))?;
    Ok(crate::inference::InferenceEngine::models_dir()?.join(entry.filename))
}

/// Whether a catalog model's file is already present in the models directory.
pub fn is_installed(id: &str) -> bool {
    model_path(id).map(|path| path.exists()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_ids_are_unique() {
        let mut ids: Vec<&str> = ENTRIES.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), ENTRIES.len());
    }

    #[test]
    fn entry_filenames_are_unique() {
        let mut names: Vec<&str> = ENTRIES.iter().map(|e| e.filename).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ENTRIES.len());
    }

    #[test]
    fn sha256_hashes_are_well_formed_lowercase_hex() {
        for entry in ENTRIES {
            assert_eq!(entry.sha256.len(), 64, "{}", entry.id);
            assert!(
                entry
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}",
                entry.id
            );
        }
    }

    #[test]
    fn default_model_is_in_the_catalog() {
        assert!(find(DEFAULT_MODEL_ID).is_some());
    }

    #[test]
    fn download_url_uses_pinned_revision_not_main() {
        let entry = find(DEFAULT_MODEL_ID).unwrap();
        let url = entry.download_url();
        assert!(url.contains(entry.revision));
        assert!(!url.contains("/resolve/main/"));
    }

    #[test]
    fn every_entry_pins_a_commit_and_a_nonempty_file_size() {
        for entry in ENTRIES {
            assert_eq!(
                entry.revision.len(),
                40,
                "{} must pin a full commit hash, not a branch",
                entry.id
            );
            assert!(
                entry
                    .revision
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} revision must be lowercase hex",
                entry.id
            );
            assert!(entry.size_bytes > 0, "{} has no pinned size", entry.id);
        }
    }

    #[test]
    fn every_entrys_variant_has_capability_info() {
        for entry in ENTRIES {
            let info = variant_info(entry.variant).unwrap_or_else(|| {
                panic!("{} variant {} has no VariantInfo", entry.id, entry.variant)
            });
            assert!(!info.family.is_empty(), "{} has no family", entry.id);
            assert!(
                !info.languages.is_empty(),
                "{} advertises no languages",
                entry.id
            );
        }
    }

    #[test]
    fn variant_info_has_no_duplicate_or_unused_entries() {
        let mut variants: Vec<&str> = VARIANTS.iter().map(|info| info.variant).collect();
        variants.sort_unstable();
        variants.dedup();
        assert_eq!(variants.len(), VARIANTS.len(), "duplicate VariantInfo");

        // Every VariantInfo must be reachable from at least one catalog entry,
        // so a family change cannot leave dead capability data behind.
        for info in VARIANTS {
            assert!(
                ENTRIES.iter().any(|entry| entry.variant == info.variant),
                "VariantInfo {} matches no catalog entry",
                info.variant
            );
        }
    }

    #[test]
    fn the_non_whisper_families_are_in_the_catalog() {
        for (variant, family) in [
            ("parakeet-tdt-0.6b-v2", "parakeet"),
            ("moonshine-base", "moonshine"),
            ("canary-1b-v2", "canary"),
            ("cohere-transcribe-03-2026", "cohere_asr"),
        ] {
            let info = variant_info(variant).unwrap();
            assert_eq!(info.family, family);
            assert!(
                ENTRIES.iter().any(|entry| entry.variant == variant),
                "{variant} should have catalog entries"
            );
        }
    }

    #[test]
    fn language_summaries_track_each_family() {
        assert_eq!(
            variant_info("parakeet-tdt-0.6b-v2")
                .unwrap()
                .language_summary(),
            "English"
        );
        assert_eq!(
            variant_info("moonshine-base").unwrap().language_summary(),
            "English"
        );
        assert_eq!(
            variant_info("canary-1b-v2").unwrap().language_summary(),
            "25 languages"
        );
        assert_eq!(
            variant_info("cohere-transcribe-03-2026")
                .unwrap()
                .language_summary(),
            "14 languages"
        );
        assert!(!variant_info("parakeet-tdt-0.6b-v2")
            .unwrap()
            .is_multilingual());
        assert!(variant_info("canary-1b-v2").unwrap().is_multilingual());
    }

    #[test]
    fn hard_cap_families_are_canary_and_cohere_only() {
        for info in VARIANTS {
            match info.long_form {
                LongForm::HardCap { cap_seconds } => {
                    assert!(cap_seconds > 0);
                    assert!(
                        matches!(info.family, "canary" | "cohere_asr"),
                        "{} unexpectedly hard-caps input",
                        info.variant
                    );
                }
                LongForm::SoftWindow { window_seconds } => {
                    assert_eq!(info.family, "moonshine");
                    assert!(window_seconds > 0);
                }
                LongForm::Unbounded => {}
            }
        }
    }
}
