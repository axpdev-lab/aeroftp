# AeroShare (Peer-to-Peer Transfer)

> _Last updated: 2026-10-26_
>
> **Beta preview.** AeroShare first shipped as a Beta preview in v4.1.0 and remains Beta in v4.2.5. The transport is hardened and decentralized by design, while parts of the surface and the notifications are still being refined. Expect rough edges and please report them.

AeroShare lets you send a file or a folder straight to another person, end-to-end encrypted, without a hosted storage account. Instead of uploading to a hosted account and sharing a link to it, AeroShare moves bytes between the two devices over an end-to-end encrypted channel, directly when connectivity permits or through a relay when necessary. AeroFTP does not operate a hosted storage or file-proxy service; third-party relays can forward encrypted packets.

## How it works

AeroShare is built on [iroh](https://www.iroh.computer/) 1.0. Peers connect over QUIC. The default discovery mode, `both`, combines Mainline DHT, n0 discovery and LAN discovery. Settings offers `both`, `dht`, `n0`, `lan` and `none`. Relays provide a path when direct connectivity is unavailable. Discovery services see network/discovery metadata; end-to-end encryption protects file contents. The channel is end-to-end encrypted (a 256-bit class), so the relay, if one is used, only forwards opaque packets and never sees your files. Peers do not need an AeroFTP-hosted storage account; discovery and relay availability depend on the selected network services.

A transfer is described by a share link (an iroh ticket carrying the dial addresses and the namespace). When you share a folder, AeroShare gives you a link to send to your friend; they paste it into their AeroShare to receive. Shared folders are browse and download only on the receiving side.

## Getting started

The AeroShare interface is available at launch; background receiving remains opt-in. You will see:

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
- **No AeroFTP-hosted storage or proxy**: there is no hosted account and no AeroFTP server storing or proxying your files. Discovery follows the selected DHT/n0/LAN mode; relay services can forward encrypted traffic when needed.
- **Receiving is opt-in**: your device does not accept incoming transfers until you opt in, and you can turn it off from the status bar or Settings at any time.
- **No provider credentials involved**: AeroShare moves files directly between people and does not require sharing any storage-provider credential, so it stays within AeroFTP's broader no-credential-exposure model (see [Credential Isolation](CREDENTIAL-ISOLATION.md)).

## Status and roadmap

AeroShare remains a Beta preview in v4.2.5. The P2P stack migrated to iroh 1.0 in v4.1.0. Historical dependency evaluations do not replace the release's current dependency audit. More of the surface, the notifications and the contact workflow are being refined, and a CLI entry point is planned. See the [roadmap](../ROADMAP.md) for what is next.
