// Lists the on-screen and off-screen windows owned by one process id.
//
// The Bar measurement uses this read-only query to prove that the Bar panel
// exists in the "on" arm (a window of the configured height) and that no
// window exists in the "off" arm. It needs no Screen Recording permission
// because it reads only owner pid, layer, bounds and the on-screen flag, never
// window names or contents. The source is checked with `swiftc -typecheck`.
//
// Usage: swift list-windows.swift <pid>
// Prints one JSON array: [{"layer":0,"x":0,"y":0,"width":1,"height":1,"onScreen":true}].
// Exit codes: 0 listed, 64 usage, 70 the window list could not be read.

import CoreGraphics
import Foundation

let arguments = CommandLine.arguments
guard arguments.count == 2, let ownerPid = Int32(arguments[1]), ownerPid > 0 else {
    FileHandle.standardError.write(Data("usage: list-windows.swift <pid>\n".utf8))
    exit(64)
}

guard
    let raw = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]]
else {
    FileHandle.standardError.write(Data("the window list could not be read\n".utf8))
    exit(70)
}

var windows: [[String: Any]] = []

for window in raw {
    guard let pid = window[kCGWindowOwnerPID as String] as? Int32, pid == ownerPid else {
        continue
    }
    let bounds = window[kCGWindowBounds as String] as? [String: Any] ?? [:]
    windows.append([
        "layer": window[kCGWindowLayer as String] as? Int ?? 0,
        "x": bounds["X"] as? Double ?? 0,
        "y": bounds["Y"] as? Double ?? 0,
        "width": bounds["Width"] as? Double ?? 0,
        "height": bounds["Height"] as? Double ?? 0,
        "onScreen": (window[kCGWindowIsOnscreen as String] as? Bool) ?? false,
    ])
}

let data = try JSONSerialization.data(withJSONObject: windows, options: [.sortedKeys])
print(String(decoding: data, as: UTF8.self))
