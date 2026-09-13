/** Create an object-based session from the initialized wasm-bindgen constructor. */
export function createLanguageSession(LanguageSession) {
  const session = new LanguageSession();
  return {
    dispatch(method, params) {
      return JSON.parse(session.dispatch(method, JSON.stringify(params)));
    },
    dispatch_with_context(method, params) {
      return JSON.parse(session.dispatch_with_context(method, JSON.stringify(params)));
    },
    free() {
      session.free();
    },
  };
}
