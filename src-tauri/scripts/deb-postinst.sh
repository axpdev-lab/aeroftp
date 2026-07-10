#!/bin/bash
# Post-install script for AeroFTP Linux packages
# Copies AeroFTP MIME type icons (AeroVault, AeroFTP profile, AeroFTP keystore)
# to the active icon themes (Yaru, Adwaita, etc.) and updates icon/MIME caches.

set -e

ROOT="${AEROFTP_PACKAGE_ROOT:-}"
root_path() {
    printf '%s%s\n' "$ROOT" "$1"
}

BIN_DIR="$(root_path /usr/bin)"
LIB_DIR="$(root_path /usr/lib/aeroftp)"
GUI_BIN="$LIB_DIR/aeroftp.bin"
DISPATCH_SRC="$LIB_DIR/aeroftp-dispatch"
DISPATCH_BIN="$BIN_DIR/aeroftp"
CLI_ALIAS="$BIN_DIR/aeroftp-cli"
AFTP_ALIAS="$BIN_DIR/aftp"

mkdir -p "$BIN_DIR" "$LIB_DIR"

if [ ! -x "$DISPATCH_SRC" ]; then
    echo "ERROR: dispatcher payload not found at $DISPATCH_SRC" >&2
    exit 1
fi
if [ ! -x "$LIB_DIR/aeroftp-cli" ]; then
    echo "ERROR: CLI payload not found at $LIB_DIR/aeroftp-cli" >&2
    exit 1
fi

# Tauri installs the GUI payload as /usr/bin/aeroftp. Move that real GUI
# payload under /usr/lib/aeroftp and put the dispatcher at /usr/bin/aeroftp.
if [ -f "$DISPATCH_BIN" ] && ! cmp -s "$DISPATCH_BIN" "$DISPATCH_SRC"; then
    mv -f "$DISPATCH_BIN" "$GUI_BIN"
    chmod 0755 "$GUI_BIN"
fi

install -m 0755 "$DISPATCH_SRC" "$DISPATCH_BIN"
ln -sfn aeroftp "$AFTP_ALIAS"
ln -sfn aeroftp "$CLI_ALIAS"

# Tauri bundles every [[bin]] target into /usr/bin. The .deb and AppImage
# are rewritten at build time (patch-deb-dispatch.sh / patch-appimage-
# dispatch.sh) to drop the secondary binaries, but the .rpm has no build-
# time patch and relies on this scriptlet. Remove the leaked helper/test
# binaries here so every Linux format converges on the same /usr/bin
# surface (dispatcher + aftp + aeroftp-cli symlinks only). Idempotent:
# on .deb/AppImage these are already gone, so rm -f is a no-op.
for stray in aeroftp-dispatch aerorsync_serve seed_test_profiles seed_axpbuntu_lab; do
    rm -f "$BIN_DIR/$stray"
done

HICOLOR="$(root_path /usr/share/icons/hicolor)"
ICON_NAMES="application-x-aerovault application-x-aeroftp application-x-aeroftp-keystore application-x-aerozip application-x-aeroftp-script"
SIZES="16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512"

# Detect active icon themes from all user accounts
copy_to_theme() {
    local THEME_DIR
    THEME_DIR="$(root_path "/usr/share/icons/$1")"
    [ -d "$THEME_DIR" ] || return 0

    for ICON_NAME in $ICON_NAMES; do
        for SIZE in $SIZES; do
            if [ -d "$THEME_DIR/$SIZE/mimetypes" ] && [ -f "$HICOLOR/$SIZE/mimetypes/$ICON_NAME.png" ]; then
                cp -f "$HICOLOR/$SIZE/mimetypes/$ICON_NAME.png" "$THEME_DIR/$SIZE/mimetypes/$ICON_NAME.png"
            fi
            # HiDPI @2x variants (Yaru)
            if [ -d "$THEME_DIR/${SIZE}@2x/mimetypes" ] && [ -f "$HICOLOR/$SIZE/mimetypes/$ICON_NAME.png" ]; then
                cp -f "$HICOLOR/$SIZE/mimetypes/$ICON_NAME.png" "$THEME_DIR/${SIZE}@2x/mimetypes/$ICON_NAME.png"
            fi
        done
        if [ -d "$THEME_DIR/scalable/mimetypes" ] && [ -f "$HICOLOR/scalable/mimetypes/$ICON_NAME.svg" ]; then
            cp -f "$HICOLOR/scalable/mimetypes/$ICON_NAME.svg" "$THEME_DIR/scalable/mimetypes/$ICON_NAME.svg"
        fi
    done

    gtk-update-icon-cache -f -q "$THEME_DIR" 2>/dev/null || true
}

# Copy to common Linux desktop themes
for THEME in Yaru Yaru-dark Adwaita elementary Papirus Papirus-Dark Papirus-Light Breeze breeze-dark; do
    copy_to_theme "$THEME"
done

# Patch Tauri-generated desktop file: add MimeType and %f argument
# Tauri 2 does not propagate fileAssociations to the .desktop file
for DESKTOP_FILE in \
    "$(root_path /usr/share/applications/AeroFTP.desktop)" \
    "$(root_path /usr/share/applications/com.aeroftp.AeroFTP.desktop)" \
    "$(root_path /usr/share/applications/aeroftp.desktop)"
do
    [ -f "$DESKTOP_FILE" ] || continue

    # Route desktop launches and file associations through the dispatcher.
    if grep -q '^Exec=' "$DESKTOP_FILE"; then
        sed -i 's|^Exec=.*|Exec=/usr/bin/aeroftp %U|' "$DESKTOP_FILE"
    else
        echo 'Exec=/usr/bin/aeroftp %U' >> "$DESKTOP_FILE"
    fi

    # Add MimeType if missing (covers all 5 AeroFTP file formats + URL schemes)
    if ! grep -q '^MimeType=' "$DESKTOP_FILE"; then
        echo 'MimeType=application/x-aerovault;application/x-aeroftp;application/x-aeroftp-keystore;application/x-aerozip;application/x-aeroftp-script;x-scheme-handler/ftp;x-scheme-handler/ftps;x-scheme-handler/sftp;' >> "$DESKTOP_FILE"
    fi

    # Tauri's bundle category is a fixed macOS-style enum (Utility is the closest
    # value), so the generated desktop file ships Categories=Utility;. A transfer
    # client belongs under Network;FileTransfer;. Rewrite it here (last word for
    # installed .deb/.rpm, since both share this postinstall) rather than
    # reintroducing a hand-maintained desktop file.
    if grep -q '^Categories=' "$DESKTOP_FILE"; then
        sed -i 's|^Categories=.*|Categories=Network;FileTransfer;|' "$DESKTOP_FILE"
    else
        echo 'Categories=Network;FileTransfer;' >> "$DESKTOP_FILE"
    fi
    if ! grep -q '^Keywords=' "$DESKTOP_FILE"; then
        echo 'Keywords=ftp;sftp;ftps;webdav;s3;transfer;file;sync;cloud;encryption;' >> "$DESKTOP_FILE"
    fi
done

if [ -z "$ROOT" ]; then
    # Update hicolor cache and MIME database
    gtk-update-icon-cache -f -q "$HICOLOR" 2>/dev/null || true
    update-mime-database /usr/share/mime 2>/dev/null || true
    update-desktop-database /usr/share/applications 2>/dev/null || true

    # Register AeroFTP as default handler for all 5 MIME types
    xdg-mime default AeroFTP.desktop application/x-aerovault 2>/dev/null || true
    xdg-mime default AeroFTP.desktop application/x-aeroftp 2>/dev/null || true
    xdg-mime default AeroFTP.desktop application/x-aeroftp-keystore 2>/dev/null || true
    xdg-mime default AeroFTP.desktop application/x-aerozip 2>/dev/null || true
    xdg-mime default AeroFTP.desktop application/x-aeroftp-script 2>/dev/null || true
fi

exit 0
