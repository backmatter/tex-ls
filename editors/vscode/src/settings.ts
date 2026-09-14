import * as path from 'node:path';

interface Configuration {
  get<T>(key: string): T | undefined;
}

// Forward only server settings. Client-only settings must never become protocol inputs.
export function serverSettings(config: Configuration) {
  return {
    lineWidth: config.get<number>('lineWidth'),
    indentWidth: config.get<number>('indentWidth'),
    texmf: {
      enabled: config.get<boolean>('texmf.enabled'),
      roots: config.get<string[]>('texmf.roots'),
      useKpsewhich: config.get<boolean>('texmf.useKpsewhich'),
      explicitOnly: config.get<boolean>('texmf.explicitOnly'),
    },
    diagnostics: { compiler: config.get<boolean>('diagnostics.compiler') },
    outline: config.get<object>('outline'),
    inlayHints: config.get<object>('inlayHints'),
  };
}

export function serverCommand(extensionPath: string, override: string, platform = process.platform): string {
  if (override.trim()) {
    // Preserve spaces in executable paths. No shell expansion or argument splitting.
    return override;
  }
  return path.join(extensionPath, 'server', platform === 'win32' ? 'tex-ls.exe' : 'tex-ls');
}

export async function withCurrentDocument<T>(
  document: { readonly version: number; readonly isClosed: boolean },
  token: { readonly isCancellationRequested: boolean },
  request: () => T | PromiseLike<T>,
): Promise<T | null> {
  if (document.isClosed || token.isCancellationRequested) return null;
  const version = document.version;
  const result = await request();
  return document.isClosed || document.version !== version || token.isCancellationRequested
    ? null : result;
}
