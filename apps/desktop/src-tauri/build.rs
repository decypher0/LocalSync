fn main() {
    // Round 25 investigation: real testing after rounds 20/21 (centered
    // layout, design tokens, icons, theming) reported the running app
    // still looking like the pre-round-20 unstyled page. Directly tested
    // in this sandbox before touching anything: the committed source is
    // correct (confirmed present - .app-shell, every design token, all 30
    // icon symbols); a genuinely clean local `cargo build` correctly
    // re-embeds a modified frontend file (proven: editing only styles.css
    // triggered a real recompile) and a newly-added one (proven the same
    // way); a same-inputs rebuild afterward was a real no-op (ruling out
    // "it always recompiles regardless" as a false positive on the two
    // tests above). So this is not a source bug, and not a plain,
    // uncached local build bug either.
    //
    // What's left, and can't be fully ruled out from this sandbox: the
    // release pipeline (.github/workflows/release.yml) restores a cached
    // target/ via Swatinem/rust-cache, keyed on Cargo.lock/Cargo.toml -
    // not on frontend file content. A cache restore's file mtimes don't
    // necessarily land in the same relative order a normal edit-then-
    // rebuild sequence would produce, which is a real, if unproven here,
    // risk for any mtime-sensitive staleness check. This line is
    // defensive hardening against exactly that class of risk: an explicit
    // rerun-if-changed on the whole frontend directory costs nothing (the
    // dev builds tested above already behaved correctly without it) and
    // directly closes the one gap this investigation couldn't fully
    // exercise outside a real CI run.
    println!("cargo:rerun-if-changed=../src");
    tauri_build::build()
}
