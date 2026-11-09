import type {
  AppInfo,
  ConfigFileView,
  HostStatus,
  ModuleStatusView,
} from '../src/sdk/bindings.gen';

export const appInfo: AppInfo = {
  name: 'Stache Next',
  version: '0.25.0',
  identifier: 'dev.stache.next',
};

export function hostStatus(overrides: Partial<HostStatus> = {}): HostStatus {
  return {
    ready: true,
    error: null,
    access: { state: 'open', revoked: false, appPath: null },
    modules: [],
    config: [],
    ...overrides,
  };
}

export function moduleStatus(overrides: Partial<ModuleStatusView> = {}): ModuleStatusView {
  return {
    id: 'proxy-audio',
    state: 'running',
    enabled: true,
    paused: false,
    revision: '0',
    detail: null,
    diagnostic: null,
    ...overrides,
  };
}

export function configFile(overrides: Partial<ConfigFileView> = {}): ConfigFileView {
  return {
    file: 'global.toml',
    path: '/tmp/stache/global.toml',
    state: 'valid',
    active: true,
    diagnostic: null,
    warnings: [],
    ...overrides,
  };
}
