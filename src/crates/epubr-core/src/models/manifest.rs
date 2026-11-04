//! Assets from the immutable Docling release `models-v1-f8a5d4fd8b08`.
//! The upstream SHA256SUMS digest is
//! `f8a5d4fd8b08b8e0a8768746046d833279e3b181eed04aec43a0eef02c0d284d`.

pub(super) struct Asset {
    pub(super) name: &'static str,
    pub(super) path: &'static str,
    pub(super) sha256: &'static str,
    pub(super) bytes: u64,
}

pub(super) const STANDARD: &[Asset] = &[
    Asset {
        name: "MODELS_NOTICE.md",
        path: "MODELS_NOTICE.md",
        sha256: "2b334a854da0ebe7eb0a64be71233a61b73ee39e3cd77a71b71f2809913fc526",
        bytes: 11_459,
    },
    Asset {
        name: "models.json",
        path: "models.json",
        sha256: "ffe84af6729843fc6666a77c628b801c6170b655858651e509081430f84dd740",
        bytes: 54_178,
    },
    Asset {
        name: "export_record.json",
        path: "export_record.json",
        sha256: "903740ec555190a59b9f11d4d679e2cc72d6625c04860a1065e65fccc036e029",
        bytes: 26_883,
    },
    Asset {
        name: "LICENSE-Apache-2.0.txt",
        path: "LICENSE-Apache-2.0.txt",
        sha256: "074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff",
        bytes: 10_280,
    },
    Asset {
        name: "LICENSE-BSD-3-Clause.txt",
        path: "LICENSE-BSD-3-Clause.txt",
        sha256: "ae8fd386099f71444a5defbbe93764e40f6182dc71ac6be31ee839498f6e8b25",
        bytes: 1_485,
    },
    Asset {
        name: "LICENSE-CDLA-Permissive-2.0.txt",
        path: "LICENSE-CDLA-Permissive-2.0.txt",
        sha256: "4531a67d443284d93ffed0803df5b10634aff21c3d77e381f2d48af01d875868",
        bytes: 2_354,
    },
    Asset {
        name: "LICENSE-MIT.txt",
        path: "LICENSE-MIT.txt",
        sha256: "b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5",
        bytes: 1_078,
    },
    Asset {
        name: "LICENSE-pdfium-binaries.txt",
        path: "LICENSE-pdfium-binaries.txt",
        sha256: "ba26c1263131696b86c10496b5066b918a20b7161822a80c274d0080105f6c93",
        bytes: 1_177,
    },
    Asset {
        name: "layout_heron_int8.onnx",
        path: "layout_heron_int8.onnx",
        sha256: "387be1f67bd58ec7fc575c53966492890d91af0810866bb383af45a18cc66161",
        bytes: 68_695_321,
    },
    Asset {
        name: "encoder_fp16.onnx",
        path: "tableformer/encoder_fp16.onnx",
        sha256: "3fb23e039b635ad34ac8a8984d9521e6bb596bd8bac2be1ce16b02deb4fb52d8",
        bytes: 54_040_666,
    },
    Asset {
        name: "decoder_kv_layers.onnx",
        path: "tableformer/decoder_kv_layers.onnx",
        sha256: "59846d2639dff5d18c2d356c77f629465d9dde8c63ab48d39a92d32609e94a99",
        bytes: 248_935,
    },
    Asset {
        name: "decoder_kv.onnx.data",
        path: "tableformer/decoder_kv.onnx.data",
        sha256: "e71523a8b7603f983088b04c9d5cb4203be117e4a0e5950028ae46a9e98103eb",
        bytes: 115_580_928,
    },
    Asset {
        name: "bbox.onnx",
        path: "tableformer/bbox.onnx",
        sha256: "a3cd88ae198bd8ae3338fbb89cf2f054474c964ae5001611727db7240eaf1125",
        bytes: 52_234,
    },
    Asset {
        name: "bbox.onnx.data",
        path: "tableformer/bbox.onnx.data",
        sha256: "e936773730de30a2d10528f69e064fd9fd222259aa65078c421f3186b5b7e36a",
        bytes: 39_618_560,
    },
    Asset {
        name: "ocr_det.onnx",
        path: "ocr_det.onnx",
        sha256: "090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f",
        bytes: 9_929_594,
    },
    Asset {
        name: "ocr_rec_v6.onnx",
        path: "ocr_rec_v6.onnx",
        sha256: "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884",
        bytes: 21_234_383,
    },
    Asset {
        name: "ocr_rec_v6_dict.txt",
        path: "ocr_rec_v6_dict.txt",
        sha256: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
        bytes: 74_947,
    },
];

pub(super) const FORMULA: &[Asset] = &[
    Asset {
        name: "cf_vision.onnx",
        path: "code_formula/vision.onnx",
        sha256: "047535cdcd37b66db65dfe9153acdef90004b49de4acfa006806607f0c7096fb",
        bytes: 374_146_399,
    },
    Asset {
        name: "cf_embed.onnx",
        path: "code_formula/embed.onnx",
        sha256: "d33ae06c232f267a7ab82454b7ab2f6db474db0f6e68de4823c855245a4e58f7",
        bytes: 231_506_178,
    },
    Asset {
        name: "cf_decoder_kv_int8.onnx",
        path: "code_formula/decoder_kv_int8.onnx",
        sha256: "3061ba36d84e24d1ab66cccd8ff55a65f85906d9640b042ea8dce259ba6ce5c5",
        bytes: 164_870_131,
    },
    Asset {
        name: "cf_tokenizer.json",
        path: "code_formula/tokenizer.json",
        sha256: "5ec5d3780a2e01b140cf34f023eb600263855a99aacdc3eb0f2600d7b1b4ac96",
        bytes: 7_160_031,
    },
];
