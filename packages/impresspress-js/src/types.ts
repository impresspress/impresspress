/**
 * SDK-level configuration. Every wire shape lives next to the service that
 * speaks it — there is no separate "types" tree, and no generated DB-row
 * layer: the SDK talks to HTTP handlers, not to tables, and the handlers'
 * projections are what a consumer can rely on.
 */
export interface ImpresspressConfig {
	url: string;
	/** URL for the auth UI (login page). Defaults to `url` if not specified. */
	authUrl?: string;
	apiKey?: string;
	headers?: Record<string, string>;
	/**
	 * Default request timeout in milliseconds. `NO_TIMEOUT` (0) disables it.
	 * Defaults to 30 s. Byte transfers (`storage.uploadFile` /
	 * `storage.downloadFile`) opt out of it by default — see `TransferOptions`.
	 */
	timeout?: number;
}
