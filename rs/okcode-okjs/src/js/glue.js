import { analyzeOKSource } from 'okjs/tooling/analyze.js';
// Returns the analyser's own result as JSON, or {"$error": message} when it throws.
globalThis.__okjs_analyze = (path, source, baseURL) => {
    try {
        return JSON.stringify(analyzeOKSource({ path, source, baseURL, importMap: null }));
    } catch (e) {
        return JSON.stringify({ $error: String((e && e.message) || e) });
    }
};
