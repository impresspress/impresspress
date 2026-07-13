// Simulates the shape of sql.js's minified UMD bundle, which embeds the
// wasm filename at multiple call sites (locateFile fallback + inline
// checks). The integration test must exercise the multi-occurrence path.
var a = "sql-wasm.wasm";
var b = locate("sql-wasm.wasm") || "sql-wasm.wasm";
