// Restores one default CoreAudio device by exact name.
//
// The measurement tool uses this only after it detects that a measured launch
// changed a default device despite the inert pilot configuration. It is never
// part of a normal run. The source is checked with `swiftc -typecheck`.
//
// Usage: swift restore-default-audio-device.swift input|output <device name>
// Exit codes: 0 restored, 64 usage, 66 device not found, 70 CoreAudio failure.

import CoreAudio
import Foundation

let arguments = CommandLine.arguments
guard arguments.count == 3, arguments[1] == "input" || arguments[1] == "output" else {
    FileHandle.standardError.write(
        Data("usage: restore-default-audio-device.swift input|output <device name>\n".utf8)
    )
    exit(64)
}

let isInput = arguments[1] == "input"
let targetName = arguments[2]

var devicesAddress = AudioObjectPropertyAddress(
    mSelector: kAudioHardwarePropertyDevices,
    mScope: kAudioObjectPropertyScopeGlobal,
    mElement: kAudioObjectPropertyElementMain
)
var devicesSize: UInt32 = 0
guard
    AudioObjectGetPropertyDataSize(
        AudioObjectID(kAudioObjectSystemObject),
        &devicesAddress,
        0,
        nil,
        &devicesSize
    ) == noErr
else {
    FileHandle.standardError.write(Data("failed to size the audio device list\n".utf8))
    exit(70)
}

let deviceCount = Int(devicesSize) / MemoryLayout<AudioDeviceID>.size
var devices = [AudioDeviceID](repeating: 0, count: deviceCount)
guard
    AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &devicesAddress,
        0,
        nil,
        &devicesSize,
        &devices
    ) == noErr
else {
    FileHandle.standardError.write(Data("failed to read the audio device list\n".utf8))
    exit(70)
}

func deviceName(_ device: AudioDeviceID) -> String? {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioObjectPropertyName,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var name: CFString = "" as CFString
    var size = UInt32(MemoryLayout<CFString>.size)
    guard AudioObjectGetPropertyData(device, &address, 0, nil, &size, &name) == noErr else {
        return nil
    }
    return name as String
}

for device in devices where deviceName(device) == targetName {
    let selector =
        isInput
        ? kAudioHardwarePropertyDefaultInputDevice
        : kAudioHardwarePropertyDefaultOutputDevice
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var value = device
    let status = AudioObjectSetPropertyData(
        AudioObjectID(kAudioObjectSystemObject),
        &address,
        0,
        nil,
        UInt32(MemoryLayout<AudioDeviceID>.size),
        &value
    )
    guard status == noErr else {
        FileHandle.standardError.write(Data("CoreAudio rejected the default device\n".utf8))
        exit(70)
    }
    print("restored default \(arguments[1]) device to \(targetName)")
    exit(0)
}

FileHandle.standardError.write(Data("audio device not found: \(targetName)\n".utf8))
exit(66)
