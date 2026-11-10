# Vendored MediaRemote adapter

Read when updating, auditing or repackaging the Bar media adapter. Nothing in this
directory is Stache code; it is third-party material shipped unmodified.

- **Upstream:** `mediaremote-adapter` by Jonas van den Berg,
  <https://github.com/ungive/mediaremote-adapter>.
- **License:** BSD 3-Clause. The header of `mediaremote-adapter.pl` states
  `Copyright (c) 2025 Jonas van den Berg` and `licensed under the BSD 3-Clause License`.
  The standard text follows; it was reproduced from that header and the SPDX
  `BSD-3-Clause` template, not copied from an upstream `LICENSE` file. Replace it with the
  upstream file when the artifacts are next refreshed.
- **Imported from:** the archived legacy tree (`_legacy/app/native/resources/`), first
  committed in `e0b4648`, copied byte-for-byte with `cp -R`. The runtime never reads
  `_legacy`.
- **Architecture:** arm64 only (`MediaRemoteAdapterTestClient` and
  `MediaRemoteAdapter.framework/Versions/A/MediaRemoteAdapter` are thin arm64 Mach-O).
  The app reports `unsupportedArchitecture` on other hosts without spawning anything.
- **Layout kept:** `Versions/A`, `Versions/Current -> A` and the top-level
  `MediaRemoteAdapter` and `Resources` links are stored in git as symlinks. Tauri's
  `bundle.resources` flattens them in the `.app` (see `docs/features/bar.md`).

| File                                                       | SHA-256                                                            |
| ---------------------------------------------------------- | ------------------------------------------------------------------ |
| `mediaremote-adapter.pl`                                   | `d97802e46db9535e2549e178c105ebf417a0254b3929fc32f08ecfd14d49a85f` |
| `MediaRemoteAdapterTestClient`                             | `3a66f7299b2111c7495870bfb35f9ba7b1b78dbfb442cbdc3c9e635c4dfedc47` |
| `MediaRemoteAdapter.framework/Versions/A/MediaRemoteAdapter` | `afc984aaf440b08fbc17412b9eab3be8abce93cbfa8ae915329fde401482acd8` |

## Feasibility (2026-11-10, macOS 27.0, arm64)

The approved probe ran `/usr/bin/perl <abs>/mediaremote-adapter.pl <abs>/MediaRemoteAdapter.framework
<abs>/MediaRemoteAdapterTestClient stream --no-diff` against these exact files. It
streamed newline-delimited JSON (`{"type":"data","diff":false,"payload":{...}}`), showed
no permission prompt, spawned no child processes (perl loads the framework in-process)
and ended on `SIGTERM` with the child reaped. Relative paths fail ("Failed to load
framework"), so callers must pass absolute paths. Largest observed line was about 157 KB
(base64 artwork). This is evidence for macOS 27 only; macOS 26 is not separately verified.

## License text

Copyright (c) 2025 Jonas van den Berg

Redistribution and use in source and binary forms, with or without modification, are
permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this list of
   conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this list of
   conditions and the following disclaimer in the documentation and/or other materials
   provided with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may be used
   to endorse or promote products derived from this software without specific prior
   written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY
EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE
COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR
TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
