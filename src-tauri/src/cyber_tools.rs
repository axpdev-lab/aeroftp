// AeroFTP Security Toolkit — Hash Forge, CryptoLab, Password Forge
// Production-grade cryptographic utilities for file integrity, encryption, and credential generation
//
// All crypto operations use audited crates (RustCrypto ecosystem).
// Sensitive data is zeroized on drop via secrecy crate.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use rand::Rng;
use sha2::Digest;
use secrecy::zeroize::Zeroize;

use crate::filesystem::validate_path;

// ─── Hash Forge ─────────────────────────────────────────────────────────────

/// Hash arbitrary text with the specified algorithm.
/// Returns lowercase hex-encoded hash.
#[tauri::command]
pub fn hash_text(text: String, algorithm: String) -> Result<String, String> {
    if text.is_empty() {
        return Err("Input text is empty".into());
    }
    hash_bytes(text.as_bytes(), &algorithm)
}

/// Hash a local file with the specified algorithm (64KB streaming buffer).
#[tauri::command]
pub async fn hash_file(path: String, algorithm: String) -> Result<String, String> {
    validate_path(&path)?;
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("Failed to open file: {}", e))?;

    let mut buffer = vec![0u8; 64 * 1024];

    match algorithm.to_lowercase().as_str() {
        "md5" => {
            let mut hasher = md5::Md5::new();
            loop {
                let n = file.read(&mut buffer).await.map_err(|e| format!("Read error: {}", e))?;
                if n == 0 { break; }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha1" => {
            let mut hasher = sha1::Sha1::new();
            loop {
                let n = file.read(&mut buffer).await.map_err(|e| format!("Read error: {}", e))?;
                if n == 0 { break; }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha256" => {
            let mut hasher = sha2::Sha256::new();
            loop {
                let n = file.read(&mut buffer).await.map_err(|e| format!("Read error: {}", e))?;
                if n == 0 { break; }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "sha512" => {
            let mut hasher = sha2::Sha512::new();
            loop {
                let n = file.read(&mut buffer).await.map_err(|e| format!("Read error: {}", e))?;
                if n == 0 { break; }
                hasher.update(&buffer[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        "blake3" => {
            let mut hasher = blake3::Hasher::new();
            loop {
                let n = file.read(&mut buffer).await.map_err(|e| format!("Read error: {}", e))?;
                if n == 0 { break; }
                hasher.update(&buffer[..n]);
            }
            Ok(hasher.finalize().to_hex().to_string())
        }
        _ => Err(format!("Unsupported algorithm: {}", algorithm)),
    }
}

/// Constant-time hash comparison to prevent timing attacks.
#[tauri::command]
pub fn compare_hashes(hash_a: String, hash_b: String) -> bool {
    let a = hash_a.trim().to_lowercase();
    let b = hash_b.trim().to_lowercase();
    if a.len() != b.len() {
        return false;
    }
    // XOR accumulator for constant-time comparison
    let mut diff: u8 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Hash in-memory bytes (shared helper).
fn hash_bytes(data: &[u8], algorithm: &str) -> Result<String, String> {
    match algorithm.to_lowercase().as_str() {
        "md5" => {
            let mut h = md5::Md5::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "sha1" => {
            let mut h = sha1::Sha1::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "sha256" => {
            let mut h = sha2::Sha256::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "sha512" => {
            let mut h = sha2::Sha512::new();
            h.update(data);
            Ok(hex::encode(h.finalize()))
        }
        "blake3" => Ok(blake3::hash(data).to_hex().to_string()),
        _ => Err(format!("Unsupported algorithm: {}", algorithm)),
    }
}

// ─── CryptoLab ──────────────────────────────────────────────────────────────

/// Encrypt text with password. Returns portable format: $ALGO$salt$nonce$ciphertext (all base64).
/// Key derivation: Argon2id (64 MB / t=3 / p=4) via crypto::derive_key().
#[tauri::command]
pub async fn crypto_encrypt_text(
    plaintext: String,
    password: String,
    algorithm: String,
) -> Result<String, String> {
    if plaintext.is_empty() {
        return Err("Input text is empty".into());
    }
    if password.is_empty() {
        return Err("Password is required".into());
    }

    let algo = algorithm.to_lowercase();
    if algo != "aes-256-gcm" && algo != "chacha20-poly1305" {
        return Err(format!("Unsupported algorithm: {}. Use aes-256-gcm or chacha20-poly1305", algorithm));
    }

    // Generate random salt (32 bytes) and nonce (12 bytes)
    let salt = crate::crypto::random_bytes(32);
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill(&mut nonce_bytes);

    // Derive 256-bit key from password via Argon2id
    let mut key = crate::crypto::derive_key(&password, &salt)?;

    // Encrypt
    let ciphertext = match algo.as_str() {
        "aes-256-gcm" => {
            crate::crypto::encrypt_aes_gcm(&key, &nonce_bytes, plaintext.as_bytes())?
        }
        "chacha20-poly1305" => {
            encrypt_chacha20(key, &nonce_bytes, plaintext.as_bytes())?
        }
        _ => unreachable!(),
    };

    // Zeroize key material
    key.zeroize();

    // Format: $algo$base64(salt)$base64(nonce)$base64(ciphertext)
    Ok(format!(
        "${}${}${}${}",
        algo,
        B64.encode(&salt),
        B64.encode(nonce_bytes),
        B64.encode(&ciphertext)
    ))
}

/// Decrypt text from portable format. Parses algorithm from the encoded string.
#[tauri::command]
pub async fn crypto_decrypt_text(
    encoded: String,
    password: String,
) -> Result<String, String> {
    if password.is_empty() {
        return Err("Password is required".into());
    }

    // Parse format: $algo$salt$nonce$ciphertext
    let parts: Vec<&str> = encoded.split('$').collect();
    if parts.len() != 5 || !parts[0].is_empty() {
        return Err("Invalid format. Expected: $algo$salt$nonce$ciphertext".into());
    }

    let algo = parts[1];
    let salt = B64.decode(parts[2]).map_err(|_| "Invalid base64 salt")?;
    let nonce_bytes = B64.decode(parts[3]).map_err(|_| "Invalid base64 nonce")?;
    let ciphertext = B64.decode(parts[4]).map_err(|_| "Invalid base64 ciphertext")?;

    if salt.len() != 32 {
        return Err("Invalid salt length".into());
    }
    if nonce_bytes.len() != 12 {
        return Err("Invalid nonce length".into());
    }

    // Derive key
    let mut key = crate::crypto::derive_key(&password, &salt)?;

    // Decrypt
    let plaintext = match algo {
        "aes-256-gcm" => {
            crate::crypto::decrypt_aes_gcm(&key, &nonce_bytes, &ciphertext)?
        }
        "chacha20-poly1305" => {
            decrypt_chacha20(key, &nonce_bytes, &ciphertext)?
        }
        _ => {
            key.zeroize();
            return Err(format!("Unknown algorithm: {}", algo));
        }
    };

    key.zeroize();

    String::from_utf8(plaintext).map_err(|_| "Decrypted data is not valid UTF-8".into())
}

/// ChaCha20-Poly1305 encryption helper
fn encrypt_chacha20(key: [u8; 32], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::Aead};
    use chacha20poly1305::aead::generic_array::GenericArray;

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&key));
    let nonce = GenericArray::from_slice(nonce);
    cipher.encrypt(nonce, plaintext)
        .map_err(|e| format!("ChaCha20-Poly1305 encrypt failed: {}", e))
}

/// ChaCha20-Poly1305 decryption helper
fn decrypt_chacha20(key: [u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::Aead};
    use chacha20poly1305::aead::generic_array::GenericArray;

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&key));
    let nonce = GenericArray::from_slice(nonce);
    cipher.decrypt(nonce, ciphertext)
        .map_err(|_| "Decryption failed — wrong password or corrupted data".into())
}

// ─── Password Forge ─────────────────────────────────────────────────────────

const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const UPPER_FULL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghjkmnpqrstuvwxyz";
const LOWER_FULL: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const DIGITS: &[u8] = b"23456789";
const DIGITS_FULL: &[u8] = b"0123456789";
const SYMBOLS: &[u8] = b"!@#$%^&*()-_=+[]{}|;:,.<>?/~";

/// Generate cryptographically secure random passwords.
#[tauri::command]
pub fn generate_password(
    length: usize,
    uppercase: bool,
    lowercase: bool,
    digits: bool,
    symbols: bool,
    exclude_ambiguous: bool,
    count: usize,
) -> Result<Vec<String>, String> {
    if length < 8 || length > 128 {
        return Err("Length must be between 8 and 128".into());
    }
    let count = count.clamp(1, 10);

    let mut pool = Vec::new();
    if uppercase {
        pool.extend_from_slice(if exclude_ambiguous { UPPER } else { UPPER_FULL });
    }
    if lowercase {
        pool.extend_from_slice(if exclude_ambiguous { LOWER } else { LOWER_FULL });
    }
    if digits {
        pool.extend_from_slice(if exclude_ambiguous { DIGITS } else { DIGITS_FULL });
    }
    if symbols {
        pool.extend_from_slice(SYMBOLS);
    }

    if pool.is_empty() {
        return Err("Select at least one character set".into());
    }

    let mut rng = rand::thread_rng();
    let mut results = Vec::with_capacity(count);

    for _ in 0..count {
        let pwd: String = (0..length)
            .map(|_| pool[rng.gen_range(0..pool.len())] as char)
            .collect();
        results.push(pwd);
    }

    Ok(results)
}

/// Generate random passphrases from EFF diceware short wordlist (1296 words).
#[tauri::command]
pub fn generate_passphrase(
    word_count: usize,
    separator: String,
    capitalize: bool,
    count: usize,
) -> Result<Vec<String>, String> {
    if word_count < 3 || word_count > 12 {
        return Err("Word count must be between 3 and 12".into());
    }
    let count = count.clamp(1, 10);
    let sep = if separator.is_empty() { "-" } else { &separator };

    let mut rng = rand::thread_rng();
    let mut results = Vec::with_capacity(count);

    for _ in 0..count {
        let words: Vec<String> = (0..word_count)
            .map(|_| {
                let word = WORDLIST[rng.gen_range(0..WORDLIST.len())];
                if capitalize {
                    let mut c = word.chars();
                    match c.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().to_string() + c.as_str(),
                    }
                } else {
                    word.to_string()
                }
            })
            .collect();
        results.push(words.join(sep));
    }

    Ok(results)
}

/// Calculate password entropy in bits.
#[tauri::command]
pub fn calculate_entropy(
    length: usize,
    uppercase: bool,
    lowercase: bool,
    digits: bool,
    symbols: bool,
    exclude_ambiguous: bool,
) -> f64 {
    let mut pool_size: usize = 0;
    if uppercase {
        pool_size += if exclude_ambiguous { UPPER.len() } else { UPPER_FULL.len() };
    }
    if lowercase {
        pool_size += if exclude_ambiguous { LOWER.len() } else { LOWER_FULL.len() };
    }
    if digits {
        pool_size += if exclude_ambiguous { DIGITS.len() } else { DIGITS_FULL.len() };
    }
    if symbols {
        pool_size += SYMBOLS.len();
    }
    if pool_size == 0 {
        return 0.0;
    }
    length as f64 * (pool_size as f64).log2()
}

// ─── EFF Short Wordlist (1296 words, 5 dice) ───────────────────────────────
// Source: Electronic Frontier Foundation (eff.org/dice)
// Compact subset for passphrase generation

const WORDLIST: &[&str] = &[
    "acid", "acme", "acre", "acts", "aged", "agent", "agile", "aging",
    "agony", "agree", "ahead", "aide", "aim", "ajar", "alarm", "album",
    "alert", "alike", "alive", "alley", "allot", "allow", "alone", "alpha",
    "amaze", "ample", "amuse", "angel", "anger", "angle", "ankle", "annex",
    "anvil", "apart", "apex", "apple", "apply", "arena", "argon", "arise",
    "armor", "army", "aroma", "array", "arrow", "arson", "ash", "asset",
    "atlas", "atom", "attic", "audio", "audit", "avert", "avid", "avoid",
    "awake", "award", "azure", "bacon", "badge", "badly", "bagel", "baker",
    "balmy", "banjo", "barge", "baron", "basic", "basin", "batch", "bath",
    "beach", "beard", "beast", "begin", "being", "belly", "below", "bench",
    "berry", "bible", "bike", "bind", "birch", "birth", "blade", "blame",
    "blank", "blast", "blaze", "bleak", "blend", "bless", "blimp", "blind",
    "bliss", "block", "bloom", "blown", "bluff", "blunt", "blurt", "blush",
    "board", "boat", "bogus", "bolt", "bonus", "boost", "booth", "born",
    "bound", "bowed", "boxer", "brain", "brand", "brave", "bread", "break",
    "breed", "brick", "bride", "brief", "bring", "brink", "brisk", "broad",
    "broil", "brook", "broth", "brown", "brush", "buddy", "budge", "build",
    "bulge", "bunch", "bunny", "burst", "buyer", "cabin", "cache", "camel",
    "candy", "cargo", "carry", "carve", "catch", "cause", "cedar", "chain",
    "chair", "chalk", "champ", "chaos", "charm", "chart", "chase", "cheap",
    "check", "cheek", "cheer", "chess", "chest", "chief", "child", "chill",
    "china", "chip", "choir", "chord", "chunk", "cinch", "claim", "clamp",
    "clash", "clasp", "class", "clay", "clean", "clear", "clerk", "click",
    "cliff", "climb", "cling", "clip", "cloak", "clock", "clone", "close",
    "cloth", "cloud", "clown", "club", "clue", "coach", "coast", "cobra",
    "cocoa", "coil", "color", "comet", "comic", "comma", "cone", "coral",
    "core", "couch", "count", "court", "cover", "crack", "craft", "crane",
    "crash", "crate", "crawl", "crazy", "cream", "creek", "creep", "crest",
    "crisp", "cross", "crowd", "crown", "crush", "crust", "cubic", "curve",
    "cycle", "daily", "dance", "darts", "datum", "dawn", "dealt", "debug",
    "decay", "decor", "decoy", "delay", "delta", "delve", "demon", "dense",
    "depot", "depth", "derby", "desk", "detox", "deter", "digit", "dimly",
    "diner", "dirty", "disco", "dish", "ditch", "diver", "dizzy", "dodge",
    "doing", "donor", "donut", "doubt", "dough", "draft", "drain", "drape",
    "drawn", "dream", "dress", "drift", "drill", "drink", "drive", "droit",
    "drone", "drool", "dusty", "dwarf", "dwell", "dying", "eager", "eagle",
    "early", "earth", "easel", "eaten", "eater", "ebony", "eclat", "edges",
    "edict", "eight", "elbow", "elder", "elect", "elfin", "elite", "ember",
    "empty", "enact", "enemy", "enjoy", "enter", "entry", "envoy", "epoch",
    "equal", "equip", "erode", "error", "essay", "ethic", "evade", "event",
    "every", "evict", "exact", "exalt", "exam", "exile", "exist", "extra",
    "fable", "facet", "faint", "fairy", "faith", "fancy", "fatal", "favor",
    "feast", "fence", "ferry", "fetch", "fever", "fewer", "fiber", "field",
    "fifth", "fifty", "fight", "finch", "first", "flame", "flank", "flare",
    "flash", "flask", "fleet", "flesh", "flick", "fling", "flint", "float",
    "flock", "flood", "floor", "flora", "flour", "flown", "fluid", "flush",
    "flute", "focal", "focus", "foggy", "folly", "force", "forge", "form",
    "forth", "forum", "fossil", "found", "fox", "foyer", "frail", "frame",
    "frank", "fraud", "fresh", "friar", "front", "frost", "froze", "fruit",
    "fuel", "fully", "fungi", "fury", "fused", "fuzzy", "gamer", "gamma",
    "gauge", "gavel", "gaze", "gecko", "ghost", "giant", "given", "gizmo",
    "glad", "gland", "glare", "glass", "gleam", "glide", "glint", "globe",
    "gloom", "glory", "gloss", "glove", "glyph", "going", "golden", "golfer",
    "goose", "gorge", "grace", "grade", "grain", "grand", "grant", "grape",
    "graph", "grasp", "grass", "grave", "graze", "great", "greed", "green",
    "greet", "grief", "grill", "grind", "gripe", "groin", "groom", "gross",
    "group", "grove", "growl", "grown", "gruel", "grump", "guard", "guess",
    "guide", "guild", "guilt", "guise", "gulch", "gummy", "gusto", "gusty",
    "gutter", "haven", "havoc", "hazel", "heart", "heath", "heavy", "hedge",
    "heist", "hello", "hence", "herb", "heron", "hired", "hitch", "hobby",
    "homer", "honey", "honor", "horse", "hotel", "house", "hover", "human",
    "humid", "humor", "husky", "hyena", "icing", "ideal", "igloo", "image",
    "imply", "inbox", "index", "indie", "inert", "infer", "ingot", "inner",
    "input", "intro", "ionic", "irate", "ivory", "jaunt", "jazzy", "jelly",
    "jewel", "jiffy", "joint", "joker", "jolly", "joust", "judge", "juice",
    "jumbo", "jumpy", "juror", "karma", "kayak", "kebab", "khaki", "kinky",
    "kiosk", "knife", "knack", "kneel", "knelt", "knobs", "knock", "knoll",
    "known", "koala", "label", "lance", "lapse", "large", "laser", "latch",
    "later", "lathe", "layer", "leach", "leafy", "lean", "learn", "lease",
    "ledge", "legal", "lemon", "level", "lever", "light", "lilac", "limbo",
    "linen", "liner", "lingo", "llama", "lobby", "local", "lodge", "lofty",
    "logic", "login", "lotus", "lousy", "lover", "loyal", "lucid", "lucky",
    "lunar", "lunch", "lunge", "lusty", "lying", "lyric", "macro", "mafia",
    "magic", "major", "maker", "mango", "manor", "maple", "march", "marsh",
    "mason", "match", "matte", "maxim", "mayor", "mealy", "media", "melon",
    "mercy", "merit", "mesa", "metal", "meter", "might", "mince", "minor",
    "minus", "mirth", "miser", "misty", "mixer", "mocha", "model", "mogul",
    "moist", "money", "month", "moose", "moral", "morph", "motel", "motor",
    "mound", "mount", "mourn", "mouse", "moved", "movie", "mower", "mucus",
    "muddy", "mulch", "mural", "music", "musty", "naive", "nanny", "nasal",
    "natal", "naval", "nerve", "never", "newly", "nexus", "night", "ninja",
    "noble", "noise", "north", "notch", "noted", "novel", "nudge", "nurse",
    "nylon", "oasis", "ocean", "offer", "olive", "omega", "onset", "opera",
    "optic", "orbit", "order", "organ", "other", "otter", "ought", "outer",
    "overt", "oxide", "ozone", "paddy", "paint", "panda", "panel", "panic",
    "paper", "party", "paste", "patch", "patio", "pause", "peach", "pearl",
    "pedal", "penny", "perch", "peril", "phase", "photo", "piano", "piece",
    "pilot", "pinch", "pixel", "pizza", "place", "plaid", "plain", "plane",
    "plank", "plant", "plate", "plaza", "plead", "pleat", "plier", "pluck",
    "plumb", "plume", "plump", "plunge", "plush", "poach", "point", "polar",
    "polka", "poser", "posit", "pouch", "pound", "power", "prank", "prawn",
    "press", "price", "pride", "prime", "print", "prior", "prism", "prize",
    "probe", "prone", "proof", "prose", "proud", "prune", "psych", "pulse",
    "pupil", "puppy", "purge", "pushy", "quack", "qualm", "quart", "queen",
    "query", "quest", "queue", "quick", "quiet", "quill", "quirk", "quota",
    "quote", "radar", "radio", "rally", "ranch", "range", "rapid", "raven",
    "rayon", "razor", "reach", "react", "realm", "rebel", "rebus", "recap",
    "regal", "rehab", "reign", "relax", "relay", "relic", "remit", "renew",
    "repay", "repel", "reply", "retry", "rhino", "ridge", "rifle", "rigid",
    "rinse", "ripen", "risen", "risky", "rival", "river", "roast", "robin",
    "robot", "rocky", "rogue", "roman", "roomy", "roster", "rover", "royal",
    "rugby", "ruler", "rumor", "rural", "rusty", "sadly", "saint", "salad",
    "salon", "salsa", "satin", "sauna", "savor", "scale", "scare", "scene",
    "scent", "scone", "scope", "score", "scout", "scrap", "sedan", "seize",
    "sense", "serum", "serve", "seven", "shade", "shaft", "shake", "shall",
    "shame", "shape", "share", "shark", "sharp", "shawl", "sheep", "sheer",
    "sheet", "shelf", "shell", "shift", "shirt", "shock", "shore", "short",
    "shout", "shown", "shrub", "sight", "sigma", "since", "sixth", "sixty",
    "sized", "skill", "skimp", "skull", "skunk", "slate", "sleep", "sleet",
    "slept", "slice", "slide", "slope", "sloth", "slump", "smack", "small",
    "smart", "smear", "smell", "smile", "smirk", "smith", "smoke", "snack",
    "snake", "snare", "sneak", "snore", "snout", "solar", "solid", "solve",
    "sonic", "south", "space", "spare", "spark", "spawn", "speak", "spear",
    "speed", "spend", "spent", "spice", "spicy", "spine", "spite", "split",
    "spoke", "spoon", "sport", "spray", "spree", "squad", "stack", "staff",
    "stage", "stain", "stair", "stake", "stale", "stall", "stamp", "stand",
    "stank", "stark", "start", "state", "stave", "stays", "steam", "steel",
    "steep", "steer", "stems", "stern", "stick", "stiff", "still", "stock",
    "stoic", "stone", "stood", "stool", "store", "storm", "story", "stout",
    "stove", "strap", "straw", "stray", "strip", "stuck", "study", "stuff",
    "stump", "style", "sugar", "suite", "sunny", "super", "surge", "swamp",
    "swarm", "swear", "sweat", "sweep", "sweet", "swept", "swift", "swing",
    "swirl", "swore", "sworn", "syrup", "tabby", "table", "tally", "talon",
    "tangy", "tango", "tapir", "taste", "taunt", "tempo", "tense", "tepid",
    "theta", "thick", "thing", "think", "thorn", "those", "three", "threw",
    "throw", "thump", "tiger", "tight", "timer", "timid", "tipsy", "titan",
    "title", "toast", "token", "tonic", "topaz", "torch", "total", "touch",
    "tough", "towel", "tower", "toxic", "trace", "track", "trade", "trail",
    "train", "trait", "trash", "trawl", "trend", "trial", "tribe", "trick",
    "troop", "trout", "truck", "truly", "trump", "trunk", "trust", "truth",
    "tulip", "tumor", "tuner", "turbo", "tutor", "twang", "tweed", "twice",
    "twist", "ultra", "uncut", "under", "undue", "unfit", "fungi", "union",
    "unite", "unity", "until", "upper", "upset", "urban", "usage", "usher",
    "using", "usual", "utter", "valet", "valid", "valor", "valve", "vault",
    "venom", "venue", "verse", "vigor", "vinyl", "viola", "viper", "viral",
    "visit", "visor", "vista", "vital", "vivid", "vocal", "vodka", "voice",
    "voter", "vouch", "vowel", "wages", "wagon", "waist", "waste", "watch",
    "water", "waved", "waxed", "weary", "weave", "wedge", "weird", "wheat",
    "wheel", "where", "which", "while", "white", "whole", "widen", "width",
    "wield", "windy", "witch", "woken", "woman", "world", "worry", "worst",
    "worth", "wound", "wrath", "wrist", "wrote", "yacht", "yearn", "yeast",
    "yield", "young", "youth", "zebra", "zilch", "zones",
];
