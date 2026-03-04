/**
 * Generate Ed25519 keypair for AeroFTP License System.
 *
 * Usage:
 *   deno run supabase/generate-keypair.ts
 *
 * Output:
 *   - Private key (base64) → set as LICENSE_SIGNING_KEY secret in Supabase
 *   - Public key (hex bytes) → paste into license.rs PUBLIC_KEY_BYTES
 */

const keypair = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]);

// Export private key (PKCS8 format, base64 encoded)
const privateKeyDer = await crypto.subtle.exportKey("pkcs8", keypair.privateKey);
const privateKeyB64 = btoa(String.fromCharCode(...new Uint8Array(privateKeyDer)));

// Export public key (raw 32 bytes)
const publicKeyRaw = await crypto.subtle.exportKey("raw", keypair.publicKey);
const publicKeyBytes = new Uint8Array(publicKeyRaw);

// Format as Rust array literal
const rustArray = Array.from(publicKeyBytes)
  .map((b) => `0x${b.toString(16).padStart(2, "0")}`)
  .join(", ");

console.log("=== AeroFTP License Keypair ===\n");
console.log("PRIVATE KEY (set as Supabase secret LICENSE_SIGNING_KEY):");
console.log(privateKeyB64);
console.log("\nPUBLIC KEY (paste in license.rs):");
console.log(`const PUBLIC_KEY_BYTES: [u8; 32] = [${rustArray}];`);
console.log("\nPUBLIC KEY (hex):");
console.log(Array.from(publicKeyBytes).map((b) => b.toString(16).padStart(2, "0")).join(""));
