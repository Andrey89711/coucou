// API keys live in the Windows Credential Manager or, on Linux, the Secret
// Service (GNOME Keyring, KWallet) — never on disk and never in the front end — the island can only ask whether a key is present.

use keyring::Entry;

const SERVICE: &str = "fr.louisraille.coucou";
const LARGE_PART_CHARS: usize = 900;
const MAX_LARGE_PARTS: usize = 32;
const LARGE_KEYS: &[&str] = &[
    "openai-chatgpt-access-token",
    "openai-chatgpt-refresh-token",
    "openai-chatgpt-id-token",
];

/// Every key Coucou may store. Anything outside this list is refused.
pub const KNOWN_KEYS: &[&str] = &[
    "anthropic-api-key",
    "openai-api-key",
    "openai-chatgpt-meta",
    "openai-chatgpt-client-id",
    "openai-chatgpt-host-id",
    "openai-chatgpt-access-token",
    "openai-chatgpt-refresh-token",
    "openai-chatgpt-id-token",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
];

fn entry(key: &str) -> Option<Entry> {
    if !known_key(key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

fn known_key(key: &str) -> bool {
    KNOWN_KEYS.contains(&key)
        || LARGE_KEYS.iter().any(|base| {
            key == format!("{base}-parts")
                || key
                    .strip_prefix(&format!("{base}-part-"))
                    .and_then(|value| value.parse::<usize>().ok())
                    .is_some_and(|part| (1..=MAX_LARGE_PARTS).contains(&part))
        })
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}

/// Windows Credential Manager limits each UTF-16 credential blob. OAuth JWTs
/// can exceed that limit, so keep them as a bounded set of small credentials.
pub fn set_large(key: &str, value: &str) -> Result<(), String> {
    if !LARGE_KEYS.contains(&key) {
        return Err(format!("unknown large key {key}"));
    }
    if value.is_empty() {
        return clear_large(key);
    }
    let chars: Vec<char> = value.chars().collect();
    let chunks: Vec<String> = chars
        .chunks(LARGE_PART_CHARS)
        .map(|chunk| chunk.iter().collect())
        .collect();
    if chunks.len() > MAX_LARGE_PARTS {
        return Err("OAuth credential is unexpectedly large.".into());
    }
    for (index, chunk) in chunks.iter().enumerate() {
        set(&format!("{key}-part-{}", index + 1), chunk)?;
    }
    for index in (chunks.len() + 1)..=MAX_LARGE_PARTS {
        let _ = clear(&format!("{key}-part-{index}"));
    }
    set(&format!("{key}-parts"), &chunks.len().to_string())?;
    // Remove a value written by an older build before chunked storage existed.
    let _ = clear(key);
    Ok(())
}

pub fn get_large(key: &str) -> Option<String> {
    if !LARGE_KEYS.contains(&key) {
        return None;
    }
    let Some(parts) = get(&format!("{key}-parts")) else {
        return get(key);
    };
    let parts = parts.parse::<usize>().ok()?;
    if !(1..=MAX_LARGE_PARTS).contains(&parts) {
        return None;
    }
    let mut value = String::new();
    for index in 1..=parts {
        value.push_str(&get(&format!("{key}-part-{index}"))?);
    }
    Some(value)
}

pub fn clear_large(key: &str) -> Result<(), String> {
    if !LARGE_KEYS.contains(&key) {
        return Err(format!("unknown large key {key}"));
    }
    clear(key)?;
    clear(&format!("{key}-parts"))?;
    for index in 1..=MAX_LARGE_PARTS {
        clear(&format!("{key}-part-{index}"))?;
    }
    Ok(())
}
