// The Google OAuth client ID/secret are baked in at compile time via
// `option_env!` in src/oauth.rs. Rebuild whenever either variable changes so
// a release build never ships a stale (or missing) value.
fn main() {
    println!("cargo:rerun-if-env-changed=GOOGLE_OAUTH_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=GOOGLE_OAUTH_CLIENT_SECRET");
}
