//! Internal licence issuing tool (§46).
//!
//! The private key lives outside this repository and is never committed, never
//! placed in `.env`, and never shipped in the app. The app embeds only the
//! public half.
//!
//! ```text
//! license-generator keygen                       # writes a new keypair
//! license-generator issue --key private.pem ...  # signs a licence file
//! license-generator verify --pub <b64> license.json
//! ```

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use louver_core::license::{canonical_bytes, LicenseFile, LicensePayload};
use std::collections::HashMap;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let flags = parse_flags(&args);

    match cmd {
        "keygen" => keygen(&flags),
        "issue" => issue(&flags),
        "verify" => verify_cmd(&flags),
        _ => {
            eprintln!(
                "louver license-generator

  keygen [--out <dir>]
      Generate an Ed25519 keypair. The private key is written to
      <dir>/license-signing-key.txt and must never be committed.

  issue --key <file> --id <id> [--edition Lifetime] [--device <fingerprint>]
        [--expires <RFC3339>] [--max-devices 2] [--out license.json]

  verify --pub <base64> <license.json>
"
            );
            std::process::exit(2);
        }
    }
}

fn parse_flags(args: &[String]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(k) = args[i].strip_prefix("--") {
            let v = args.get(i + 1).filter(|v| !v.starts_with("--")).cloned().unwrap_or_default();
            m.insert(k.to_string(), v);
            i += 2;
        } else {
            m.entry("_positional".to_string()).or_insert_with(|| args[i].clone());
            i += 1;
        }
    }
    m
}

fn keygen(flags: &HashMap<String, String>) {
    let dir = flags.get("out").cloned().unwrap_or_else(|| ".".into());
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let priv_b64 = b64().encode(sk.to_bytes());
    let pub_b64 = b64().encode(sk.verifying_key().to_bytes());

    let path = std::path::Path::new(&dir).join("license-signing-key.txt");
    std::fs::write(&path, format!("{priv_b64}\n")).expect("failed to write the private key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    println!("private key -> {}  (KEEP OFFLINE, NEVER COMMIT)", path.display());
    println!("public key  -> {pub_b64}");
    println!();
    println!("Build the app with this public key embedded:");
    println!("  LOUVER_LICENSE_PUBLIC_KEY={pub_b64} cargo build --release");
}

fn issue(flags: &HashMap<String, String>) {
    let key_path = flags.get("key").expect("--key <private key file> is required");
    let raw = std::fs::read_to_string(key_path).expect("cannot read the private key");
    let bytes: [u8; 32] = b64()
        .decode(raw.trim())
        .expect("private key is not valid base64")
        .try_into()
        .expect("private key must be 32 bytes");
    let sk = SigningKey::from_bytes(&bytes);

    let payload = LicensePayload {
        license_id: flags
            .get("id")
            .cloned()
            .unwrap_or_else(|| format!("LL-{}", chrono::Utc::now().timestamp())),
        product: "Louver Live".into(),
        edition: flags.get("edition").cloned().unwrap_or_else(|| "Lifetime".into()),
        issued_at: chrono::Utc::now().to_rfc3339(),
        expires_at: flags.get("expires").filter(|s| !s.is_empty()).cloned(),
        device_binding: flags.get("device").filter(|s| !s.is_empty()).cloned(),
        max_devices: flags.get("max-devices").and_then(|s| s.parse().ok()).unwrap_or(2),
    };

    let file =
        LicenseFile { signature: b64().encode(sk.sign(&canonical_bytes(&payload)).to_bytes()), payload };
    let out = flags.get("out").cloned().unwrap_or_else(|| "license.json".into());
    std::fs::write(&out, serde_json::to_vec_pretty(&file).unwrap()).expect("failed to write the licence");
    println!("issued {} -> {out}", file.payload.license_id);
}

fn verify_cmd(flags: &HashMap<String, String>) {
    let pub_b64 = flags.get("pub").expect("--pub <base64 public key> is required");
    let path = flags.get("_positional").cloned().unwrap_or_else(|| "license.json".into());

    let vk_bytes: [u8; 32] = b64().decode(pub_b64).expect("bad base64").try_into().expect("32 bytes");
    let vk = VerifyingKey::from_bytes(&vk_bytes).expect("invalid public key");
    let file: LicenseFile =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("cannot read the licence"))
            .expect("bad licence json");
    let sig: [u8; 64] = b64()
        .decode(&file.signature)
        .expect("bad signature base64")
        .try_into()
        .expect("signature must be 64 bytes");

    match vk.verify(&canonical_bytes(&file.payload), &ed25519_dalek::Signature::from_bytes(&sig)) {
        Ok(()) => println!("VALID   {} · {}", file.payload.license_id, file.payload.edition),
        Err(_) => {
            println!("INVALID signature does not match");
            std::process::exit(1);
        }
    }
}
