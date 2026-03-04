/**
 * Supabase Edge Function: verify-purchase
 *
 * Verifies a Google Play purchase and returns a signed license token.
 * Called by the mobile app after a successful Google Play purchase.
 *
 * Required secrets:
 *   - LICENSE_SIGNING_KEY: Ed25519 private key (base64)
 *   - GOOGLE_PLAY_SERVICE_ACCOUNT: JSON service account credentials
 *   - SUPABASE_URL: Auto-provided
 *   - SUPABASE_SERVICE_ROLE_KEY: Auto-provided
 */

import { serve } from "https://deno.land/std@0.208.0/http/server.ts";
import { createClient } from "https://esm.sh/@supabase/supabase-js@2";
import { encode as base64url } from "https://deno.land/std@0.208.0/encoding/base64url.ts";
import { decode as base64decode } from "https://deno.land/std@0.208.0/encoding/base64.ts";

// Allowed origins: mobile app (Capacitor) and localhost for dev
const ALLOWED_ORIGINS = [
  "capacitor://localhost",      // iOS Capacitor
  "http://localhost",           // Android Capacitor
  "http://localhost:14321",     // Tauri dev
  "https://localhost:14321",
];

function getCorsHeaders(req: Request): Record<string, string> {
  const origin = req.headers.get("origin") || "";
  const allowed = ALLOWED_ORIGINS.includes(origin) ? origin : ALLOWED_ORIGINS[0];
  return {
    "Access-Control-Allow-Origin": allowed,
    "Access-Control-Allow-Methods": "POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type, Authorization",
    "Vary": "Origin",
  };
}

// Rate limiting: in-memory counter (resets on cold start)
const rateLimits = new Map<string, { count: number; resetAt: number }>();
const RATE_LIMIT = 10; // requests per minute
const RATE_WINDOW = 60_000; // 1 minute

function getClientIp(req: Request): string {
  // Supabase Edge Functions sit behind a proxy — prefer x-real-ip, then cf-connecting-ip
  return req.headers.get("x-real-ip")
    || req.headers.get("cf-connecting-ip")
    || req.headers.get("x-forwarded-for")?.split(",")[0]?.trim()
    || "unknown";
}

function checkRateLimit(ip: string): boolean {
  const now = Date.now();
  const entry = rateLimits.get(ip);
  if (!entry || now > entry.resetAt) {
    rateLimits.set(ip, { count: 1, resetAt: now + RATE_WINDOW });
    return true;
  }
  if (entry.count >= RATE_LIMIT) return false;
  entry.count++;
  return true;
}

serve(async (req: Request) => {
  const cors = getCorsHeaders(req);

  // CORS preflight
  if (req.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: cors });
  }

  if (req.method !== "POST") {
    return new Response(JSON.stringify({ error: "Method not allowed" }), {
      status: 405,
      headers: { ...cors, "Content-Type": "application/json" },
    });
  }

  // Rate limiting
  const clientIp = getClientIp(req);
  if (!checkRateLimit(clientIp)) {
    return new Response(JSON.stringify({ error: "Rate limit exceeded" }), {
      status: 429,
      headers: { ...cors, "Content-Type": "application/json" },
    });
  }

  try {
    const { purchaseToken, orderId, packageName } = await req.json();

    if (!purchaseToken || !orderId || !packageName) {
      return new Response(
        JSON.stringify({ error: "Missing required fields" }),
        { status: 400, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    // Validate package name
    if (packageName !== "it.axpdev.aeroftp.mobile") {
      return new Response(
        JSON.stringify({ error: "Invalid package" }),
        { status: 400, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    // TODO: Verify purchase with Google Play Developer API
    // This is a placeholder — Google Play verification will be implemented before
    // the license system goes live. The License tab is dev-only until then.
    //
    // const googleAuth = JSON.parse(Deno.env.get("GOOGLE_PLAY_SERVICE_ACCOUNT")!);
    // const verifyResult = await verifyWithGooglePlay(googleAuth, packageName, "aeroftp_pro_unlock", purchaseToken);

    // Initialize Supabase client
    const supabase = createClient(
      Deno.env.get("SUPABASE_URL")!,
      Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!
    );

    // Hash the purchase token for storage (privacy)
    const tokenHash = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(purchaseToken)
    );
    const purchaseTokenHash = Array.from(new Uint8Array(tokenHash))
      .slice(0, 16)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");

    // Check if this order was already processed
    const { data: existing } = await supabase
      .from("licenses")
      .select("license_key")
      .eq("order_id", orderId)
      .single();

    if (existing) {
      // Return existing license
      return new Response(
        JSON.stringify({
          licenseKey: existing.license_key,
          token: existing.license_key,
          humanReadableKey: await tokenToHumanReadable(existing.license_key),
        }),
        { status: 200, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    // Generate license subject ID
    const subId = "aero_" + crypto.randomUUID().replace(/-/g, "").slice(0, 12);

    // Create license payload
    const payload = {
      sub: subId,
      iss: "aeroftp-license",
      iat: Math.floor(Date.now() / 1000),
      exp: 0, // perpetual
      tier: "pro",
      max_devices: 5,
      order_id: orderId,
      purchase_token_hash: purchaseTokenHash,
    };

    // Sign with Ed25519
    const privateKeyB64 = Deno.env.get("LICENSE_SIGNING_KEY")!;
    const privateKeyBytes = base64decode(privateKeyB64);

    const cryptoKey = await crypto.subtle.importKey(
      "pkcs8",
      privateKeyBytes,
      { name: "Ed25519" },
      false,
      ["sign"]
    );

    const payloadBytes = new TextEncoder().encode(JSON.stringify(payload));
    const signature = await crypto.subtle.sign("Ed25519", cryptoKey, payloadBytes);

    const token = base64url(payloadBytes) + "." + base64url(new Uint8Array(signature));

    // Store in database (check for errors)
    const { error: insertError } = await supabase.from("licenses").insert({
      order_id: orderId,
      purchase_token_hash: purchaseTokenHash,
      license_key: token,
      tier: "pro",
      max_devices: 5,
    });

    if (insertError) {
      console.error("DB insert error:", insertError);
      return new Response(
        JSON.stringify({ error: "Failed to store license" }),
        { status: 500, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    return new Response(
      JSON.stringify({
        licenseKey: token,
        token: token,
        humanReadableKey: await tokenToHumanReadable(token),
      }),
      { status: 200, headers: { ...cors, "Content-Type": "application/json" } }
    );
  } catch (error) {
    console.error("verify-purchase error:", error);
    return new Response(
      JSON.stringify({ error: "Internal server error" }),
      { status: 500, headers: { ...getCorsHeaders(req), "Content-Type": "application/json" } }
    );
  }
});

async function tokenToHumanReadable(token: string): Promise<string> {
  // Must match Rust: SHA-256 of token -> first 10 bytes -> BASE32 no-pad -> first 16 chars
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token));
  const first10 = new Uint8Array(hash).slice(0, 10);
  // RFC 4648 Base32 encode (no padding), uppercase
  const base32Chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = 0;
  let value = 0;
  let encoded = "";
  for (const byte of first10) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      encoded += base32Chars[(value >>> bits) & 0x1f];
    }
  }
  if (bits > 0) {
    encoded += base32Chars[(value << (5 - bits)) & 0x1f];
  }
  const chars = encoded.slice(0, 16);
  return `AERO-${chars.slice(0, 4)}-${chars.slice(4, 8)}-${chars.slice(8, 12)}-${chars.slice(12, 16)}`;
}
