-- AeroFTP License System — Supabase Schema
-- Run this in the Supabase SQL Editor to create the required tables.

-- Licenses table: stores verified purchase records and signed license keys
CREATE TABLE IF NOT EXISTS licenses (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id TEXT UNIQUE NOT NULL,
  purchase_token_hash TEXT NOT NULL,
  license_key TEXT UNIQUE NOT NULL,
  tier TEXT NOT NULL DEFAULT 'pro',
  max_devices INTEGER NOT NULL DEFAULT 5,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  revoked BOOLEAN NOT NULL DEFAULT false
);

-- Device activations: tracks which devices have activated a license
CREATE TABLE IF NOT EXISTS device_activations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  license_id UUID NOT NULL REFERENCES licenses(id) ON DELETE CASCADE,
  device_fingerprint TEXT NOT NULL,
  device_name TEXT,
  activated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(license_id, device_fingerprint)
);

-- Indexes for common queries
CREATE INDEX IF NOT EXISTS idx_licenses_order_id ON licenses(order_id);
CREATE INDEX IF NOT EXISTS idx_licenses_license_key ON licenses(license_key);
CREATE INDEX IF NOT EXISTS idx_device_activations_license_id ON device_activations(license_id);

-- RLS policies (optional, Edge Functions bypass RLS with service role key)
ALTER TABLE licenses ENABLE ROW LEVEL SECURITY;
ALTER TABLE device_activations ENABLE ROW LEVEL SECURITY;

-- No public access — only Edge Functions with service_role can read/write
CREATE POLICY "No public access" ON licenses FOR ALL USING (false);
CREATE POLICY "No public access" ON device_activations FOR ALL USING (false);

-- Trigger to enforce max_devices at the database level (prevents TOCTOU race conditions)
CREATE OR REPLACE FUNCTION enforce_max_devices()
RETURNS TRIGGER AS $$
DECLARE
  current_count INTEGER;
  max_allowed INTEGER;
BEGIN
  SELECT count(*) INTO current_count
    FROM device_activations
    WHERE license_id = NEW.license_id;

  SELECT max_devices INTO max_allowed
    FROM licenses
    WHERE id = NEW.license_id;

  IF current_count >= max_allowed THEN
    RAISE EXCEPTION 'Maximum device activations (%) reached for this license', max_allowed;
  END IF;

  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER check_max_devices
  BEFORE INSERT ON device_activations
  FOR EACH ROW
  EXECUTE FUNCTION enforce_max_devices();
