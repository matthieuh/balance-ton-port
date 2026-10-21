//! UniFFI's bindings generator, built from the pinned `uniffi` version so the
//! generated Swift always matches the scaffolding in the static library.

fn main() {
    uniffi::uniffi_bindgen_main()
}
