/**
 * Data captured by the foundation performance measurement.
 *
 * Every external source (command output, JSON files, log lines) is parsed and
 * validated at the boundary in the module that reads it; the types below only
 * describe the already-validated result that is written to samples.json and
 * rendered into the Markdown report.
 */

export type TargetKind = 'next' | 'legacy';

export interface BuildInfo {
  executablePath: string;
  bundlePath: string | null;
  sizeBytes: number;
  modifiedAt: string;
  pathIncludesReleaseDirectory: boolean;
}

export interface Target {
  kind: TargetKind;
  bundlePath: string | null;
  bundleIdentifier: string | null;
  executablePath: string;
  build: BuildInfo;
}

export interface MachineInfo {
  model: string;
  chip: string;
  memoryBytes: number;
}

export interface OsInfo {
  productVersion: string;
  buildVersion: string;
}

export interface ToolchainInfo {
  rustc: string;
  cargo: string;
}

export interface RepositoryInfo {
  root: string;
  commit: string;
  branch: string;
  dirty: boolean;
}

export interface PowerInfo {
  source: string;
  batteryPercent: number | null;
  raw: string;
}

export interface DisplayInfo {
  gpu: string | null;
  name: string | null;
  resolution: string | null;
  main: boolean;
  mirroring: boolean;
  online: boolean;
}

export interface AudioDeviceInfo {
  name: string;
  manufacturer: string | null;
  transport: string | null;
  inputChannels: number | null;
  outputChannels: number | null;
  hdmi: boolean;
  builtIn: boolean;
  airPlay: boolean;
  defaultInput: boolean;
  defaultOutput: boolean;
}

export interface AudioState {
  devices: AudioDeviceInfo[];
  defaultInput: string | null;
  defaultOutput: string | null;
}

export interface EnvironmentSnapshot {
  machine: MachineInfo;
  os: OsInfo;
  toolchain: ToolchainInfo;
  repository: RepositoryInfo;
  power: PowerInfo;
  displays: DisplayInfo[];
  audio: AudioState;
}

export interface ConfigFile {
  path: string;
  content: string;
}

export interface LaunchRecord {
  index: number;
  cold: boolean;
  externalReadyMs: number;
  internalTrayReadyMs: number | null;
  instrumentedWebviews: number | null;
  readySource: string;
  stderrTail: string[];
}

export interface ProcessRow {
  pid: number;
  ppid: number;
  rssKiB: number;
  cpuTimeSeconds: number;
  executable: string;
}

export interface ProcessTreeSnapshot {
  takenAt: string;
  processes: ProcessRow[];
}

export interface ObservedProcess {
  pid: number;
  ppid: number;
  executable: string;
  rssKiB: number;
}

export interface IdleProcess {
  pid: number;
  ppid: number;
  executable: string;
  role: 'host' | 'descendant';
  cpuPercent: number;
  rssFinalKiB: number;
  rssPeakKiB: number;
  presentInAllSnapshots: boolean;
  exitedDuringSampling: boolean;
}

export interface IdleSample {
  durationSeconds: number;
  sampleIntervalMs: number;
  windowSeconds: number;
  host: IdleProcess;
  processes: IdleProcess[];
  treeCpuPercent: number;
  treeRssFinalKiB: number;
  treeRssPeakKiB: number;
  unattributableWebKit: ObservedProcess[];
  treeSnapshots: ProcessTreeSnapshot[];
  notes: string[];
}

export interface ScenarioResult {
  id: string;
  label: string;
  target: Target;
  configFiles: ConfigFile[];
  launches: LaunchRecord[];
  idle: IdleSample | null;
  /** False when the run stopped before this scenario finished; its data is partial. */
  complete: boolean;
  annotations: string[];
}

export interface AudioRestoreRecord {
  attempted: boolean;
  succeeded: boolean;
  detail: string;
}

export interface SignalRecord {
  at: string;
  pid: number;
  signal: string;
  reason: string;
}

export interface SafetyRecord {
  guardChecks: string[];
  tempRoots: string[];
  tempRootsRemoved: boolean;
  audioDefaultsAtStart: {
    input: string | null;
    output: string | null;
  };
  /** Null when the devices could not be read back; the end state is then unknown. */
  audioDefaultsAtEnd: {
    input: string | null;
    output: string | null;
  } | null;
  audioRestore: AudioRestoreRecord | null;
  signals: SignalRecord[];
  notes: string[];
}

export interface MeasurementRun {
  startedAt: string;
  finishedAt: string;
  durationMs: number;
  status: 'completed' | 'failed' | 'interrupted';
  /** First failure that stopped the run; cleanup issues stay in `safety.notes`. */
  failure: string | null;
  commandLine: string[];
  targets: {
    next: Target;
    legacy: Target | null;
  };
  environment: EnvironmentSnapshot;
  /** Null when the default devices could not be read back after the run. */
  audioAtEnd: AudioState | null;
  scenarios: ScenarioResult[];
  safety: SafetyRecord;
  limitations: string[];
}

export interface MeasureOptions {
  newAppPath: string;
  legacyAppPath: string | null;
  launches: number;
  idleSeconds: number;
}
