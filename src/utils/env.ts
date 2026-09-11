/**
 * Detect if the app is running in a Tauri environment
 */
export const isTauri = () => {
    return typeof window !== 'undefined' &&
        (!!(window as any).__TAURI_INTERNALS__ || !!(window as any).__TAURI__);
};

/**
 * Return the inference server address reachable from the active runtime.
 * Browser deployments use their public origin; native clients use loopback.
 */
export const getProxyBaseUrl = (port: number): string => {
    if (!isTauri()) return window.location.origin;
    return `http://127.0.0.1:${port}`;
};

/**
 * Detect if running on Linux
 */
export const isLinux = () => {
    return navigator.userAgent.toLowerCase().includes('linux');
};
