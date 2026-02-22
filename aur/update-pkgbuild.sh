#!/bin/bash
# Update PKGBUILD and .SRCINFO for a new AeroFTP release.
# Usage: ./update-pkgbuild.sh 2.6.0

set -euo pipefail

NEW_VER="${1:?Usage: $0 <version> (e.g. 2.6.0)}"
OLD_VER=$(grep '^pkgver=' PKGBUILD | cut -d= -f2)

if [[ "$NEW_VER" == "$OLD_VER" ]]; then
    echo "Already at version $NEW_VER"
    exit 0
fi

echo "Updating $OLD_VER → $NEW_VER"

# Update PKGBUILD
sed -i "s/^pkgver=.*/pkgver=$NEW_VER/" PKGBUILD
sed -i "s/^pkgrel=.*/pkgrel=1/" PKGBUILD

# Regenerate .SRCINFO
# If makepkg is available (Arch), use it; otherwise manual sed
if command -v makepkg &>/dev/null; then
    makepkg --printsrcinfo > .SRCINFO
else
    sed -i "s/pkgver = $OLD_VER/pkgver = $NEW_VER/g" .SRCINFO
    sed -i "s/$OLD_VER\.AppImage/$NEW_VER.AppImage/g" .SRCINFO
    sed -i "s/v$OLD_VER/v$NEW_VER/g" .SRCINFO
    sed -i "s/pkgrel = .*/pkgrel = 1/" .SRCINFO
    echo "WARNING: .SRCINFO updated via sed (not makepkg). Verify manually."
fi

echo "Done. Now commit and push to AUR:"
echo "  git add PKGBUILD .SRCINFO"
echo "  git commit -m 'Update to $NEW_VER'"
echo "  git push"
