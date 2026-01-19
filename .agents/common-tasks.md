# Common Tasks

## Adding a New Status Bar Widget

### Rust side (`app/native/src/modules/bar/components/`)

1. Create `widget_name.rs` with Tauri command
2. Register command in `lib.rs` under `generate_handler![]`
3. Add event constant to `events.rs`

### TypeScript side (`app/ui/renderer/bar/Status/`)

1. Create `WidgetName/` directory following [component structure](react-patterns.md#component-file-structure)
2. Add event constant to `types/tauri-events.ts`
3. Use `useTauriEventQuery` for data fetching

## Adding a New CLI Command

1. Add command to `app/native/src/cli/commands.rs`:

   ```rust
   #[derive(Subcommand)]
   pub enum Commands {
       /// Description
       NewCommand {
           #[arg(short, long)]
           option: Option<String>,
       },
   }
   ```

2. Handle in `main.rs` match statement

## Adding a New Configuration Option

1. Add field to appropriate struct in `config/types.rs`
2. Regenerate schema: `./scripts/generate-schema.sh`
3. Update `docs/sample-config.jsonc`
