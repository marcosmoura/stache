# Audio Device Management

Stache provides automatic audio device switching based on priority rules, making it easy to manage multiple audio devices.

## Features

- Automatic switching when devices connect/disconnect
- Priority-based device selection
- Flexible matching strategies (exact, contains, starts with, regex)
- Device dependency rules
- Separate input/output configuration
- AirPlay devices automatically prioritized

## Configuration

### Basic Setup

```jsonc
{
  "proxyAudio": {
    "enabled": true,
    "output": {
      "priority": [
        { "name": "AirPods Pro" },
        { "name": "MacBook Pro Speakers", "strategy": "contains" },
      ],
    },
  },
}
```

### Full Configuration

```jsonc
{
  "proxyAudio": {
    "enabled": true,
    "input": {
      "name": "Stache Virtual Input",
      "priority": [
        { "name": "AirPods Pro" },
        { "name": "AT2020", "strategy": "contains" },
        { "name": "MacBook Pro Microphone", "strategy": "contains" },
      ],
    },
    "output": {
      "name": "Stache Virtual Output",
      "bufferSize": 256,
      "priority": [
        { "name": "AirPods", "strategy": "startsWith" },
        {
          "name": "External Speakers",
          "strategy": "exact",
          "dependsOn": {
            "name": "MiniFuse",
            "strategy": "startsWith",
          },
        },
        { "name": "MacBook Pro Speakers", "strategy": "contains" },
      ],
    },
  },
}
```

## Configuration Reference

### Top-level Options

| Option    | Type      | Default | Description                    |
| --------- | --------- | ------- | ------------------------------ |
| `enabled` | `boolean` | `false` | Enable audio device management |
| `input`   | `object`  | -       | Input device configuration     |
| `output`  | `object`  | -       | Output device configuration    |

### Input/Output Options

| Option       | Type      | Default                         | Description                     |
| ------------ | --------- | ------------------------------- | ------------------------------- |
| `name`       | `string`  | `"Stache Virtual Input/Output"` | Virtual device name             |
| `bufferSize` | `integer` | `256`                           | Audio buffer size (output only) |
| `priority`   | `array`   | `[]`                            | Priority-ordered device rules   |

### Device Rule Options

| Option      | Type     | Default   | Description            |
| ----------- | -------- | --------- | ---------------------- |
| `name`      | `string` | required  | Device name or pattern |
| `strategy`  | `string` | `"exact"` | Matching strategy      |
| `dependsOn` | `object` | `null`    | Dependency rule        |

## Matching Strategies

### `exact` (Default)

Matches the exact device name (case-insensitive).

```jsonc
{ "name": "AirPods Pro" }
// or explicitly:
{ "name": "AirPods Pro", "strategy": "exact" }
```

Matches: "AirPods Pro", "airpods pro"
Does not match: "AirPods Pro 2", "My AirPods Pro"

### `contains`

Matches if device name contains the string.

```jsonc
{ "name": "MacBook", "strategy": "contains" }
```

Matches: "MacBook Pro Speakers", "MacBook Air Microphone"

### `startsWith`

Matches if device name starts with the string.

```jsonc
{ "name": "AirPods", "strategy": "startsWith" }
```

Matches: "AirPods Pro", "AirPods Max", "AirPods (3rd generation)"

### `regex`

Matches using a regular expression pattern.

```jsonc
{ "name": "^(Sony|Bose|Sennheiser).*", "strategy": "regex" }
```

Matches: "Sony WH-1000XM4", "Bose QC45", "Sennheiser HD650"

## Device Dependencies

Use `dependsOn` to only select a device when another device is present.

**Use case:** External speakers connected to an audio interface - only use the speakers if the interface is connected.

```jsonc
{
  "name": "External Speakers",
  "strategy": "exact",
  "dependsOn": {
    "name": "MiniFuse",
    "strategy": "startsWith",
  },
}
```

This rule means:

- Only select "External Speakers" if a device starting with "MiniFuse" is also connected
- If MiniFuse is disconnected, skip to the next priority device

## AirPlay Devices

AirPlay devices are automatically given the highest priority, even if not explicitly listed in your configuration.

**Behavior:**

- When an AirPlay device becomes available, it's automatically selected
- This happens regardless of your priority list
- To disable, don't use AirPlay devices

## Buffer Size

The `bufferSize` option affects audio latency (output only):

| Value | Latency  | Stability           |
| ----- | -------- | ------------------- |
| `128` | Lower    | May cause artifacts |
| `256` | Balanced | Recommended         |
| `512` | Higher   | Most stable         |

```jsonc
{
  "output": {
    "bufferSize": 128, // Low latency
  },
}
```

## CLI Commands

### List Devices

```bash
# All devices (table format)
stache audio list

# JSON format
stache audio list --json

# Input devices only
stache audio list --input

# Output devices only
stache audio list --output

# Output devices in JSON
stache audio list --output --json
```

**Example output:**

```text
┌──────────────────────────┬──────────┬─────────┐
│ Name                     │ Type     │ Default │
├──────────────────────────┼──────────┼─────────┤
│ MacBook Pro Speakers     │ Output   │ Yes     │
│ AirPods Pro              │ Output   │ No      │
│ External Speakers        │ Output   │ No      │
│ MacBook Pro Microphone   │ Input    │ Yes     │
│ AT2020USB+               │ Input    │ No      │
└──────────────────────────┴──────────┴─────────┘
```

## Device Types

Stache detects these device types:

| Type              | Examples                             |
| ----------------- | ------------------------------------ |
| AirPlay           | HomePod, Apple TV, AirPlay receivers |
| Bluetooth         | AirPods, wireless headphones         |
| USB               | USB audio interfaces, USB headsets   |
| Built-in          | MacBook speakers, MacBook microphone |
| Virtual/Aggregate | Software audio devices               |

## Example Configurations

### Podcaster Setup

Prioritize professional microphone, fall back to built-in:

```jsonc
{
  "proxyAudio": {
    "enabled": true,
    "input": {
      "priority": [
        { "name": "Shure SM7B", "strategy": "contains" },
        { "name": "Rode NT-USB", "strategy": "contains" },
        { "name": "MacBook Pro Microphone", "strategy": "contains" },
      ],
    },
  },
}
```

### Home Office Setup

Switch between AirPods (mobile) and desk speakers:

```jsonc
{
  "proxyAudio": {
    "enabled": true,
    "output": {
      "priority": [
        { "name": "AirPods", "strategy": "startsWith" },
        {
          "name": "Desk Speakers",
          "dependsOn": {
            "name": "Focusrite",
            "strategy": "startsWith",
          },
        },
        { "name": "MacBook Pro Speakers", "strategy": "contains" },
      ],
    },
  },
}
```

### Bluetooth Headphones

Support multiple Bluetooth headphones:

```jsonc
{
  "proxyAudio": {
    "enabled": true,
    "output": {
      "priority": [
        { "name": "^(Sony|Bose|Sennheiser|Jabra).*", "strategy": "regex" },
        { "name": "MacBook Pro Speakers", "strategy": "contains" },
      ],
    },
  },
}
```

## Troubleshooting

### Device not being selected

1. Run `stache audio list` to see exact device names
2. Check spelling and case sensitivity
3. Try a broader strategy like `contains`
4. Verify the device is connected and visible in System Preferences > Sound

### Wrong device selected

1. Check priority order - first match wins
2. Verify `dependsOn` requirements are met
3. Remember AirPlay devices are auto-prioritized

### Audio switching is slow

1. Reduce `bufferSize` value
2. Some devices take time to initialize - this is normal

### Dependency not working

1. Both the main device and dependency device must be connected
2. Verify dependency device name matches with `stache audio list`
3. Check the dependency's matching strategy
