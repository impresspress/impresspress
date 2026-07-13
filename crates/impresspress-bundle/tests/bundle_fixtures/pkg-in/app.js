// fake glue
export async function init() {
    const url = new URL('app_bg.wasm', import.meta.url);
    return fetch(url);
}
