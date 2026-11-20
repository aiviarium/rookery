// The declarations for the test transform's compiler, which ships no types.
declare module "@babel/preset-typescript" {
  const preset: import("@babel/core").PluginItem;
  export default preset;
}

declare module "@solidjs/babel-plugin" {
  const plugin: import("@babel/core").PluginItem;
  export default plugin;
}
