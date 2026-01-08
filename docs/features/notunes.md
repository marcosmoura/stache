# noTunes

noTunes prevents Apple Music (or iTunes on older macOS) from auto-launching when you press media keys or connect Bluetooth headphones.

## The Problem

macOS has a tendency to launch Apple Music when you:

- Press play/pause on your keyboard
- Connect Bluetooth headphones
- Use media keys on external keyboards

This is annoying if you use other music apps like Spotify or Tidal.

## The Solution

noTunes intercepts Apple Music launch attempts and:

1. Terminates Apple Music before it fully opens
2. Optionally launches your preferred music app instead

## Configuration

### Basic Setup

```jsonc
{
  "notunes": {
    "enabled": true,
    "targetApp": "spotify",
  },
}
```

### Configuration Options

| Option      | Type      | Default   | Description           |
| ----------- | --------- | --------- | --------------------- |
| `enabled`   | `boolean` | `true`    | Enable noTunes        |
| `targetApp` | `string`  | `"tidal"` | App to launch instead |

### Target App Options

| Value       | Application                           |
| ----------- | ------------------------------------- |
| `"spotify"` | Spotify (`/Applications/Spotify.app`) |
| `"tidal"`   | TIDAL (`/Applications/TIDAL.app`)     |
| `"none"`    | Don't launch any replacement          |

## Usage

Once enabled, noTunes runs automatically in the background:

1. You press a media key or connect Bluetooth headphones
2. macOS tries to launch Apple Music
3. Stache detects the launch attempt
4. Apple Music is terminated before it fully opens
5. (Optional) Your preferred app is launched instead

## Example Configurations

### Use Spotify

```jsonc
{
  "notunes": {
    "enabled": true,
    "targetApp": "spotify",
  },
}
```

### Use Tidal

```jsonc
{
  "notunes": {
    "enabled": true,
    "targetApp": "tidal",
  },
}
```

### Just Block Apple Music

Don't launch any replacement:

```jsonc
{
  "notunes": {
    "enabled": true,
    "targetApp": "none",
  },
}
```

### Disable noTunes

Allow Apple Music to launch normally:

```jsonc
{
  "notunes": {
    "enabled": false,
  },
}
```

## How It Works

noTunes monitors for Apple Music (and iTunes) launch events:

1. **Monitoring**: Watches for app launch notifications
2. **Detection**: Identifies when Music.app or iTunes.app starts
3. **Termination**: Sends terminate signal before the app fully loads
4. **Replacement**: Launches target app if configured

**Blocked apps:**

- `com.apple.Music` (Apple Music)
- `com.apple.iTunes` (iTunes on older macOS)

## Limitations

### App must be installed

The target app must be installed at the standard location:

- Spotify: `/Applications/Spotify.app`
- TIDAL: `/Applications/TIDAL.app`

### Brief flash

You may briefly see Apple Music's icon in the Dock before it's terminated. This is normal and lasts less than a second.

### Some triggers may still work

Certain deep system integrations (like Siri) may still open Apple Music. noTunes handles the common cases but can't intercept everything.

## Troubleshooting

### Apple Music still opens

1. Verify `enabled` is `true` in your config
2. Run `stache reload` to apply changes
3. Ensure Stache is running

### Target app doesn't launch

1. Verify the app is installed at `/Applications/`
2. Check the app name is spelled correctly in config
3. Try launching the target app manually first

### App launches but media doesn't play

The target app is launched, but it doesn't automatically start playback. You'll need to press play in the app. This is normal - noTunes redirects the app launch, not the media key.

### Still seeing Apple Music briefly

This is expected behavior. noTunes terminates Apple Music as quickly as possible, but there may be a brief moment where the icon appears in the Dock.

## Comparison with Other Solutions

### noTunes app

There's a standalone app also called "noTunes" that does something similar. Stache's implementation:

- Is built-in (no separate app needed)
- Offers target app selection
- Integrates with your existing Stache setup

### Removing Apple Music

You could remove Apple Music from your system, but:

- It may break other system features
- System updates may reinstall it
- Some apps depend on its presence

noTunes is the safer option.
