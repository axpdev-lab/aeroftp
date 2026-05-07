AeroFTP Portable - Windows
==========================

QUICK START
-----------

1. Extract this ZIP to any folder you can write to (Desktop, USB drive, network
   share, anywhere). NO admin rights required.

2. Double-click AeroFTP.exe to launch.

3. All your data (saved servers, AeroAgent chats, AeroVault, cache, logs)
   stays inside the AeroFTP folder under "data\". You can move the whole
   folder to another machine and pick up where you left off.


REQUIREMENTS
------------

  - Windows 10 1809+ or Windows 11
  - Microsoft Visual C++ Redistributable for Visual Studio 2015-2022 (x64)
      Download:  https://aka.ms/vs/17/release/vc_redist.x64.exe
      AeroFTP will prompt you on first launch if this is missing.
  - WebView2 Runtime (preinstalled on Windows 11 and on Windows 10 since
    2021 via Windows Update; install manually if missing)
      Download:  https://developer.microsoft.com/microsoft-edge/webview2/


AUTO-UPDATES
------------

The portable build checks GitHub Releases for updates and replaces the
.exe in place. No installer wizard, no admin prompt. After download +
Sigstore verification AeroFTP swaps the executable, restarts itself, and
cleans up the old version automatically.

The check happens at launch and every 24 hours while the app is running.
You can also trigger it manually from Help -> Check for Updates.


UNINSTALL
---------

Delete this folder. There is no registry footprint, no service, no leftover
file outside the folder. Your data dies with the folder unless you copy
"data\" out first.


SECURITY NOTES
--------------

  - This binary is signed with Sigstore (cosign keyless, GitHub Actions OIDC).
    The auto-updater verifies the signature against the GitHub Actions OIDC
    identity before installing any update.
  - Authenticode signing is on the roadmap but not yet active. Windows
    SmartScreen may show "Unknown publisher" on first launch; this is
    expected until the Authenticode certificate work lands.


SUPPORT
-------

  - Documentation:  https://aeroftp.app
  - Issues:         https://github.com/axpdev-lab/aeroftp/issues
  - Changelog:      https://github.com/axpdev-lab/aeroftp/blob/main/CHANGELOG.md


LICENSE
-------

GNU General Public License v3.0. See LICENSE.txt.
