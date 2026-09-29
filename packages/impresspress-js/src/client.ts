import { ImpresspressConfig } from './types';
import { HttpClient } from './http-client';
import { AuthService } from './services/auth.service';
import { StorageService } from './services/storage.service';
import { IAMService } from './services/iam.service';
import { ExtensionsService, CloudStorageExtension, ProductsExtension } from './services/extensions.service';

export class ImpresspressClient {
  public auth: AuthService;
  public storage: StorageService;
  public iam: IAMService;
  public extensions: ExtensionsService;

  // Extension-specific services
  public cloudStorage: CloudStorageExtension;
  public products: ProductsExtension;

  private config: ImpresspressConfig;
  /** The one transport every service on this client shares. */
  private http: HttpClient;

  constructor(config: ImpresspressConfig | string) {
    // If string is passed, treat it as URL
    if (typeof config === 'string') {
      this.config = { url: config };
    } else {
      this.config = config;
    }

    // Ensure URL doesn't have trailing slash
    this.config.url = this.config.url.replace(/\/$/, '');

    this.http = new HttpClient({
      url: this.config.url,
      apiKey: this.config.apiKey,
      headers: this.config.headers,
      timeout: this.config.timeout,
    });

    this.auth = new AuthService(this.config, this.http);
    this.storage = new StorageService(this.config, this.http);
    this.iam = new IAMService(this.config, this.http);
    this.extensions = new ExtensionsService(this.config, this.http);
    this.cloudStorage = new CloudStorageExtension(this.config, this.http);
    this.products = new ProductsExtension(this.config, this.http);

    // Note: With cookie-based auth, no token sync is needed.
    // The browser automatically sends httpOnly cookies with each request.
  }

  /**
   * Set a global API key for all services (for server-side/API key auth).
   * One assignment: every service reads the same transport.
   */
  public setApiKey(apiKey: string) {
    this.config.apiKey = apiKey;
    this.http.setApiKey(apiKey);
  }

  /**
   * Remove the global API key
   */
  public removeApiKey() {
    delete this.config.apiKey;
    this.http.removeApiKey();
  }

  /**
   * Get the current configuration
   */
  public getConfig(): ImpresspressConfig {
    return { ...this.config };
  }

  /**
   * Check if client is authenticated
   */
  public isAuthenticated(): boolean {
    return this.auth.isAuthenticated();
  }

}

// Export a factory function for convenience
export function createImpresspressClient(config: ImpresspressConfig | string): ImpresspressClient {
  return new ImpresspressClient(config);
}
