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
    // The subcommand is not a positional argument.
    let flags = parse_flags(args.get(1..).unwrap_or_default());

    let result = match cmd {
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
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// A plain message; this tool is operated by a human at a terminal, so a
/// sentence beats a panic backtrace.
type CliResult = Result<(), String>;

fn decode_b64(what: &str, s: &str) -> Result<Vec<u8>, String> {
    b64().decode(s.trim()).map_err(|e| format!("{what} is not valid base64: {e}"))
}

fn fixed<const N: usize>(what: &str, v: Vec<u8>) -> Result<[u8; N], String> {
    let n = v.len();
    v.try_into().map_err(|_| format!("{what} must be {N} bytes, got {n}"))
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

fn keygen(flags: &HashMap<String, String>) -> CliResult {
    let dir = flags.get("out").cloned().unwrap_or_else(|| ".".into());
    let sk = SigningKey::generate(&mut rand::rngs::OsRng);
    let priv_b64 = b64().encode(sk.to_bytes());
    let pub_b64 = b64().encode(sk.verifying_key().to_bytes());

    let path = std::path::Path::new(&dir).join("license-signing-key.txt");
    std::fs::write(&path, format!("{priv_b64}\n"))
        .map_err(|e| format!("cannot write the private key to {}: {e}", path.display()))?;
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
    Ok(())
}

fn issue(flags: &HashMap<String, String>) -> CliResult {
    let key_path = flags.get("key").ok_or("--key <private key file> is required")?;
    let raw = std::fs::read_to_string(key_path)
        .map_err(|e| format!("cannot read the private key {key_path}: {e}"))?;
    let sk = SigningKey::from_bytes(&fixed::<32>("private key", decode_b64("private key", &raw)?)?);

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
    let json = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&out, json).map_err(|e| format!("cannot write {out}: {e}"))?;
    println!("issued {} -> {out}", file.payload.license_id);
    Ok(())
}

fn verify_cmd(flags: &HashMap<String, String>) -> CliResult {
    let pub_b64 = flags.get("pub").ok_or("--pub <base64 public key> is required")?;
    let path = flags.get("_positional").cloned().unwrap_or_else(|| "license.json".into());

    let vk = VerifyingKey::from_bytes(&fixed::<32>("public key", decode_b64("public key", pub_b64)?)?)
        .map_err(|e| format!("invalid public key: {e}"))?;
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let file: LicenseFile =
        serde_json::from_str(&raw).map_err(|e| format!("{path} is not a licence file: {e}"))?;
    let sig = fixed::<64>("signature", decode_b64("signature", &file.signature)?)?;

    match vk.verify(&canonical_bytes(&file.payload), &ed25519_dalek::Signature::from_bytes(&sig)) {
        Ok(()) => {
            println!("VALID   {} · {}", file.payload.license_id, file.payload.edition);
            Ok(())
        }
        Err(_) => {
            println!("INVALID signature does not match");
            std::process::exit(1);
        }
    }
}
