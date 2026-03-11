#!/bin/bash
# Post-install script for AeroFTP .deb package
# Copies AeroVault MIME type icons to the active icon theme (Yaru, Adwaita, etc.)
# and updates icon/MIME caches.

set -e

HICOLOR="/usr/share/icons/hicolor"
ICON_NAME="application-x-aerovault"
SIZES="16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512"

# Detect active icon themes from all user accounts
copy_to_theme() {
    local THEME_DIR="/usr/share/icons/$1"
    [ -d "$THEME_DIR" ] || return 0

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

    gtk-update-icon-cache -f -q "$THEME_DIR" 2>/dev/null || true
}

# Copy to common Linux desktop themes
for THEME in Yaru Yaru-dark Adwaita elementary Papirus Papirus-Dark Papirus-Light Breeze breeze-dark; do
    copy_to_theme "$THEME"
done

# Update hicolor cache and MIME database
gtk-update-icon-cache -f -q "$HICOLOR" 2>/dev/null || true
update-mime-database /usr/share/mime 2>/dev/null || true
update-desktop-database /usr/share/applications 2>/dev/null || true

# Register AeroFTP as default handler for .aerovault files
xdg-mime default AeroFTP.desktop application/x-aerovault 2>/dev/null || true

exit 0
