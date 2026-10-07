import type { StaticFeature } from 'vscode-languageclient/node';

// vscode-languageclient advertises standard purposes only. The extension also
// registers these custom purposes and modifiers in package.json.
export const semanticHighlighting: StaticFeature = {
  fillClientCapabilities(capabilities) {
    const tokens = capabilities.textDocument?.semanticTokens;
    if (!tokens) return;
    // Our TextMate grammar supplies the foreign scopes that the server leaves
    // uncolored for this opt-in. This does not change source parsing.
    capabilities.experimental = { ...capabilities.experimental, texEmbeddedHighlighting: true };
    tokens.tokenTypes = [...new Set([...tokens.tokenTypes, 'texText', 'texLiteral', 'texPath', 'texMath'])];
    tokens.tokenModifiers = [...new Set([...tokens.tokenModifiers, 'texBold', 'texItalic'])];
  },
  initialize() {},
  clear() {},
  getState: () => ({ kind: 'static' }),
};
