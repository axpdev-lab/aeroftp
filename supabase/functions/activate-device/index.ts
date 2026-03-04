/**
 * Supabase Edge Function: activate-device
 *
 * Registers a device activation for a license key.
 * Enforces max_devices limit per license.
 *
 * Required secrets:
 *   - SUPABASE_URL: Auto-provided
 *   - SUPABASE_SERVICE_ROLE_KEY: Auto-provided
 */

import { serve } from "https://deno.land/std@0.208.0/http/server.ts";
import { createClient } from "https://esm.sh/@supabase/supabase-js@2";

// Allowed origins: mobile app (Capacitor) and localhost for dev
const ALLOWED_ORIGINS = [
  "capacitor://localhost",
  "http://localhost",
  "http://localhost:14321",
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

// Rate limiting
const rateLimits = new Map<string, { count: number; resetAt: number }>();
const RATE_LIMIT = 10;
const RATE_WINDOW = 60_000;

function getClientIp(req: Request): string {
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
    const { licenseKey, deviceFingerprint, deviceName } = await req.json();

    if (!licenseKey || !deviceFingerprint) {
      return new Response(
        JSON.stringify({ error: "Missing required fields" }),
        { status: 400, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    const supabase = createClient(
      Deno.env.get("SUPABASE_URL")!,
      Deno.env.get("SUPABASE_SERVICE_ROLE_KEY")!
    );

    // Find the license
    const { data: license, error: licenseError } = await supabase
      .from("licenses")
      .select("id, max_devices, revoked")
      .eq("license_key", licenseKey)
      .single();

    if (licenseError || !license) {
      return new Response(
        JSON.stringify({ error: "License not found" }),
        { status: 404, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    if (license.revoked) {
      return new Response(
        JSON.stringify({ error: "License has been revoked" }),
        { status: 403, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    // Atomic upsert: try INSERT with ON CONFLICT to avoid TOCTOU race condition.
    // If device already exists, just update last_seen.
    const { data: upserted, error: upsertError } = await supabase
      .from("device_activations")
      .upsert(
        {
          license_id: license.id,
          device_fingerprint: deviceFingerprint,
          device_name: deviceName || "Unknown device",
          last_seen: new Date().toISOString(),
        },
        { onConflict: "license_id,device_fingerprint" }
      )
      .select("id")
      .single();

    if (upsertError) {
      // If upsert failed, it might be because max_devices is exceeded.
      // Count current activations to check.
      const { count } = await supabase
        .from("device_activations")
        .select("id", { count: "exact", head: true })
        .eq("license_id", license.id);

      if ((count ?? 0) >= license.max_devices) {
        return new Response(
          JSON.stringify({
            error: "Maximum devices reached",
            max_devices: license.max_devices,
            current_count: count ?? 0,
          }),
          { status: 409, headers: { ...cors, "Content-Type": "application/json" } }
        );
      }

      // Some other DB error
      console.error("Device activation error:", upsertError);
      return new Response(
        JSON.stringify({ error: "Failed to activate device" }),
        { status: 500, headers: { ...cors, "Content-Type": "application/json" } }
      );
    }

    // Get updated count
    const { count: totalDevices } = await supabase
      .from("device_activations")
      .select("id", { count: "exact", head: true })
      .eq("license_id", license.id);

    return new Response(
      JSON.stringify({
        success: true,
        activated_devices: totalDevices ?? 1,
        max_devices: license.max_devices,
      }),
      { status: 200, headers: { ...cors, "Content-Type": "application/json" } }
    );
  } catch (error) {
    console.error("activate-device error:", error);
    return new Response(
      JSON.stringify({ error: "Internal server error" }),
      { status: 500, headers: { ...getCorsHeaders(req), "Content-Type": "application/json" } }
    );
  }
});
