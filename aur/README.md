# AeroFTP AUR Package

## First-Time Setup

### 1. Create AUR Account
Go to https://aur.archlinux.org/register and create an account.

### 2. Add SSH Key
Generate a key (if you don't have one):
```bash
ssh-keygen -t ed25519 -C "your@email.com"
```
Add the public key (`~/.ssh/id_ed25519.pub`) to your AUR account:
https://aur.archlinux.org/account → SSH Public Key

### 3. Configure SSH for AUR
Add to `~/.ssh/config`:
```
Host aur.archlinux.org
    IdentityFile ~/.ssh/id_ed25519
    User aur
```

### 4. Create the AUR Package Repository
```bash
git clone ssh://aur@aur.archlinux.org/aeroftp-bin.git
cd aeroftp-bin
```
This will be an empty repo (first time).

### 5. Copy Files and Push
```bash
cp /path/to/aur/PKGBUILD .
cp /path/to/aur/.SRCINFO .
cp /path/to/aur/aeroftp.desktop .
git add PKGBUILD .SRCINFO aeroftp.desktop
git commit -m "Initial upload: aeroftp-bin 2.5.2"
git push
```

The package will be live at: https://aur.archlinux.org/packages/aeroftp-bin

## Updating for New Releases

### Option A: On an Arch System
```bash
cd aeroftp-bin  # your AUR repo clone
# Edit PKGBUILD: update pkgver
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git commit -m "Update to X.Y.Z"
git push
```

### Option B: Using the Helper Script (any system)
```bash
cd aur/
./update-pkgbuild.sh 2.6.0
# Then copy PKGBUILD + .SRCINFO to your AUR repo clone and push
```

## Testing Locally (on Arch)
```bash
makepkg -si   # builds and installs
aeroftp       # run it
```

## File Structure
```
aur/
├── PKGBUILD            # Package build script
├── .SRCINFO            # Package metadata (required by AUR)
├── aeroftp.desktop     # Desktop entry file
├── update-pkgbuild.sh  # Version update helper
└── README.md           # This file
```

## Notes
- The `-bin` suffix means prebuilt binary (AppImage). No compilation needed.
- `WEBKIT_DISABLE_DMABUF_RENDERER=1` is set in the launcher for WebKitGTK compatibility.
- Users install with: `yay -S aeroftp-bin` or `paru -S aeroftp-bin`
- SHA256 sums are set to SKIP — consider computing them after each release for extra security.
