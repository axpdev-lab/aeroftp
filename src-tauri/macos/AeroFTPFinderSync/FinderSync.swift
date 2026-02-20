// AeroFTP FinderSync Extension
// Displays sync status badges in macOS Finder for AeroCloud-managed directories.
// Communicates with the AeroFTP Rust daemon via Unix socket (Nextcloud-compatible protocol).

import Cocoa
import FinderSync

class FinderSync: FIFinderSync {

    // MARK: - Properties

    /// Cached badge states: path → status string
    private var badgeCache: [String: String] = [:]

    /// Socket connection to AeroFTP daemon
    private var inputStream: InputStream?
    private var outputStream: OutputStream?

    /// Whether we have an active socket connection
    private var isConnected = false

    /// Reconnection timer
    private var reconnectTimer: Timer?

    /// Socket path
    private var socketPath: String {
        if let runtimeDir = ProcessInfo.processInfo.environment["XDG_RUNTIME_DIR"] {
            return "\(runtimeDir)/aerocloud/socket"
        }
        let uid = getuid()
        return "/tmp/aerocloud-\(uid)/socket"
    }

    // MARK: - Badge Identifiers

    private static let badgeOK = "OK"
    private static let badgeSync = "SYNC"
    private static let badgeError = "ERROR"
    private static let badgeConflict = "CONFLICT"
    private static let badgeNew = "NEW"
    private static let badgeIgnored = "IGNORE"

    // MARK: - Lifecycle

    override init() {
        super.init()

        // Register badge images
        let badgeImages: [(String, NSImage.Name, NSColor)] = [
            (FinderSync.badgeOK,       NSImage.statusAvailableName, .systemGreen),
            (FinderSync.badgeSync,     NSImage.statusPartiallyAvailableName, .systemBlue),
            (FinderSync.badgeError,    NSImage.statusUnavailableName, .systemRed),
            (FinderSync.badgeConflict, NSImage.statusUnavailableName, .systemOrange),
            (FinderSync.badgeNew,      NSImage.statusPartiallyAvailableName, .systemPurple),
            (FinderSync.badgeIgnored,  NSImage.statusNoneName, .systemGray),
        ]

        for (identifier, imageName, _) in badgeImages {
            if let image = NSImage(named: imageName) {
                FIFinderSyncController.default().setBadgeImage(image, label: identifier, forBadgeIdentifier: identifier)
            } else {
                // Fallback: create a simple circle badge
                let badge = createBadgeImage(color: badgeImages.first(where: { $0.0 == identifier })?.2 ?? .systemGray)
                FIFinderSyncController.default().setBadgeImage(badge, label: identifier, forBadgeIdentifier: identifier)
            }
        }

        // Connect to daemon
        connectToDaemon()

        // Start reconnection timer (every 10 seconds if disconnected)
        reconnectTimer = Timer.scheduledTimer(withTimeInterval: 10.0, repeats: true) { [weak self] _ in
            guard let self = self, !self.isConnected else { return }
            self.connectToDaemon()
        }

        NSLog("AeroFTP FinderSync: initialized")
    }

    // MARK: - FIFinderSync Overrides

    override func beginObservingDirectory(at url: URL) {
        NSLog("AeroFTP FinderSync: begin observing %@", url.path)
    }

    override func endObservingDirectory(at url: URL) {
        NSLog("AeroFTP FinderSync: end observing %@", url.path)
    }

    override func requestBadgeIdentifier(for url: URL) {
        let path = url.path

        // Check cache first
        if let cached = badgeCache[path] {
            FIFinderSyncController.default().setBadgeIdentifier(cached, for: url)
            return
        }

        // Query daemon asynchronously
        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let self = self else { return }
            let status = self.queryFileStatus(path: path)

            DispatchQueue.main.async {
                self.badgeCache[path] = status
                if status != "NOP" {
                    FIFinderSyncController.default().setBadgeIdentifier(status, for: url)
                }
            }
        }
    }

    // MARK: - Context Menu (optional)

    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        // No custom context menu items for now
        return nil
    }

    // MARK: - Socket Communication

    /// Connect to the AeroFTP daemon Unix socket
    private func connectToDaemon() {
        disconnect()

        let path = socketPath
        guard FileManager.default.fileExists(atPath: path) else {
            NSLog("AeroFTP FinderSync: socket not found at %@", path)
            return
        }

        // Create Unix domain socket
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = path.utf8CString
        guard pathBytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else {
            NSLog("AeroFTP FinderSync: socket path too long")
            return
        }
        withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
            let raw = UnsafeMutableRawPointer(ptr)
            pathBytes.withUnsafeBufferPointer { buf in
                raw.copyMemory(from: buf.baseAddress!, byteCount: buf.count)
            }
        }

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            NSLog("AeroFTP FinderSync: failed to create socket: %d", errno)
            return
        }

        let connectResult = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                Darwin.connect(fd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }

        guard connectResult == 0 else {
            NSLog("AeroFTP FinderSync: connect failed: %d", errno)
            close(fd)
            return
        }

        // Wrap in streams
        CFStreamCreatePairWithSocket(nil, fd, nil, nil)

        // Use FileHandle-based approach for Unix sockets
        let fileHandle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        isConnected = true

        // Send version handshake
        if let data = "VERSION:\n".data(using: .utf8) {
            fileHandle.write(data)
        }

        // Store file handle for queries
        objc_setAssociatedObject(self, "socketFD", NSNumber(value: fd), .OBJC_ASSOCIATION_RETAIN)

        NSLog("AeroFTP FinderSync: connected to daemon")

        // Register observed directories (sync roots will be auto-detected)
        // FIFinderSyncController.default().directoryURLs = [URL(fileURLWithPath: NSHomeDirectory())]
    }

    /// Disconnect from daemon
    private func disconnect() {
        if let fdNum = objc_getAssociatedObject(self, "socketFD") as? NSNumber {
            close(fdNum.int32Value)
            objc_setAssociatedObject(self, "socketFD", nil, .OBJC_ASSOCIATION_RETAIN)
        }
        isConnected = false
    }

    /// Query sync status for a file path via the Nextcloud-compatible protocol
    private func queryFileStatus(path: String) -> String {
        guard let fdNum = objc_getAssociatedObject(self, "socketFD") as? NSNumber else {
            return "NOP"
        }

        let fd = fdNum.int32Value
        let request = "RETRIEVE_FILE_STATUS\npath\t\(path)\ndone\n"

        guard let requestData = request.data(using: .utf8) else {
            return "NOP"
        }

        // Write request
        let written = requestData.withUnsafeBytes { ptr in
            Darwin.write(fd, ptr.baseAddress!, requestData.count)
        }

        guard written == requestData.count else {
            NSLog("AeroFTP FinderSync: write failed")
            isConnected = false
            return "NOP"
        }

        // Read response (STATUS:<state>:<path>\ndone\n)
        var buffer = [UInt8](repeating: 0, count: 4096)
        let bytesRead = Darwin.read(fd, &buffer, buffer.count)

        guard bytesRead > 0 else {
            NSLog("AeroFTP FinderSync: read failed")
            isConnected = false
            return "NOP"
        }

        let response = String(bytes: buffer[0..<bytesRead], encoding: .utf8) ?? ""
        let lines = response.split(separator: "\n")

        guard let statusLine = lines.first, statusLine.hasPrefix("STATUS:") else {
            return "NOP"
        }

        // Parse STATUS:<state>:<path>
        let parts = statusLine.split(separator: ":", maxSplits: 2)
        guard parts.count >= 2 else {
            return "NOP"
        }

        return String(parts[1])
    }

    // MARK: - Badge Image Creation

    /// Create a simple circle badge image for a given color
    private func createBadgeImage(color: NSColor) -> NSImage {
        let size = NSSize(width: 16, height: 16)
        let image = NSImage(size: size)
        image.lockFocus()

        // White border circle
        let borderRect = NSRect(x: 1, y: 1, width: 14, height: 14)
        NSColor.white.setFill()
        NSBezierPath(ovalIn: borderRect).fill()

        // Colored inner circle
        let innerRect = NSRect(x: 2, y: 2, width: 12, height: 12)
        color.setFill()
        NSBezierPath(ovalIn: innerRect).fill()

        image.unlockFocus()
        return image
    }
}
