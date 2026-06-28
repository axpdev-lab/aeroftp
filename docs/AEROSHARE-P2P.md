# AeroShare (Peer-to-Peer Transfer)

> _Last updated: 2026-06-28_

> **Beta preview.** AeroShare ships as a Beta preview in v4.1.0. The transport is hardened and decentralized by design, while parts of the surface and the notifications are still being refined. Expect rough edges and please report them.

AeroShare lets you send a file or a folder straight to another person, end-to-end encrypted, with no server in the middle. Instead of uploading to a hosted account and sharing a link to it, AeroShare opens a direct peer-to-peer channel between the two devices and moves the bytes across it. Nothing is stored on an AeroFTP server, because there is no AeroFTP server in the path.

## How it works

AeroShare is built on [iroh](https://www.iroh.computer/) 1.0. Two peers find each other through the decentralized Mainline DHT and connect over a QUIC channel, with federated relays used only as a fallback when a direct connection cannot be established. The channel is end-to-end encrypted (a 256-bit class), so the relay, if one is used, only forwards opaque packets and never sees your files. There is no central tie to any operator-run server: every user is a node.

A transfer is described by a share link (an iroh ticket carrying the dial addresses and the namespace). When you share a folder, AeroShare gives you a link to send to your friend; they paste it into their AeroShare to receive. Shared folders are browse and download only on the receiving side.

## Getting started

AeroShare is always-on at launch in v4.1.0. You will see:

- A **Discover** tile with a 256-bit E2E badge.
- A **+friend** button in the titlebar, to the right of the theme toggle, which opens the AeroShare hub.
- The **AeroShare hub** dialog, which you can drag by its header.
- A **status-bar receiver indicator** next to AeroCloud that shows whether your device is currently listening for incoming transfers, and toggles it on click.

The feature auto-activates the first time you add a friend or share a folder, so there is nothing to enable in Settings beforehand. Adding a friend triggers a one-time prompt:

> AeroShare is on. You added a friend, so AeroShare is now active. Do you want to receive files your friends send you? You can change this anytime in Settings.

This keeps the standing receive loop **opt-in**: you decide when your device accepts incoming transfers. Both the activation and the receive setting are changeable from Settings at any time.

## Sending

- **Send a file**: right-click a file and choose **Send via AeroShare**, then pick a friend.
- **Share a folder**: share the folder to get a link, then send that link back to your friend. They paste it into AeroShare to receive the folder (browse and download only).

## Receiving

Incoming transfers land in the **AeroShare Inbox**, grouped per sender. When the standing receiver is on (the status-bar indicator is lit), your device listens for transfers from your contacts. Each sender has their own inbox area, so received items stay organized by who sent them.

## Friends (address book)

AeroShare keeps a small address book of saved contacts, your AeroShare friends. Pick them when sending, and with auto-accept enabled, receive from them without a prompt each time. Saved contacts make repeat exchanges with the same person a single click.

## Privacy and trust

- **End-to-end encrypted**: only the two peers can read the transfer; relays forward ciphertext only.
- **No operator server in the path**: there is no hosted account and no AeroFTP server storing or proxying your files. Discovery is decentralized (Mainline DHT) and relays are a neutral, federated fallback.
- **Receiving is opt-in**: your device does not accept incoming transfers until you opt in, and you can turn it off from the status bar or Settings at any time.
- **No provider credentials involved**: AeroShare moves files directly between people and does not require sharing any storage-provider credential, so it stays within AeroFTP's broader no-credential-exposure model (see [Credential Isolation](CREDENTIAL-ISOLATION.md)).

## Status and roadmap

AeroShare is a Beta preview in v4.1.0. The P2P stack was migrated to iroh 1.0 for this release, which also cleared the dependency advisories the previous line carried. More of the surface, the notifications and the contact workflow are being refined, and a CLI entry point is planned. See the [roadmap](../ROADMAP.md) for what is next.
