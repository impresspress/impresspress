import { ImpresspressConfig } from './types';
import { AuthService } from './services/auth.service';
import { StorageService } from './services/storage.service';
import { DatabaseService } from './services/database.service';
import { IAMService } from './services/iam.service';
import { ExtensionsService, CloudStorageExtension, ProductsExtension } from './services/extensions.service';

export class ImpresspressClient {
  public auth: AuthService;
  public storage: StorageService;
  public database: DatabaseService;
  public iam: IAMService;
  public extensions: ExtensionsService;
  
  // Extension-specific services
  public cloudStorage: CloudStorageExtension;
  public products: ProductsExtension;

  private config: ImpresspressConfig;

  constructor(config: ImpresspressConfig | string) {
    // If string is passed, treat it as URL
    if (typeof config === 'string') {
      this.config = { url: config };
    } else {
      this.config = config;
    }

    // Ensure URL doesn't have trailing slash
    this.config.url = this.config.url.replace(/\/$/, '');

    // Initialize services
    this.auth = new AuthService(this.config);
    this.storage = new StorageService(this.config);
    this.database = new DatabaseService(this.config);
    this.iam = new IAMService(this.config);
    this.extensions = new ExtensionsService(this.config);
    
    // Initialize extension-specific services
    this.cloudStorage = new CloudStorageExtension(this.config);
    this.products = new ProductsExtension(this.config);

    // Note: With cookie-based auth, no token sync is needed.
    // The browser automatically sends httpOnly cookies with each request.
  }

  /**
   * Set a global API key for all services (for server-side/API key auth)
   */
  public setApiKey(apiKey: string) {
    this.config.apiKey = apiKey;
    const services = [
      this.auth,
      this.storage,
      this.database,
      this.iam,
      this.extensions,
      this.cloudStorage,
      this.products,
    ];
    services.forEach(service => service.setApiKey(apiKey));
  }

  /**
   * Remove the global API key
   */
  public removeApiKey() {
    delete this.config.apiKey;
    const services = [
      this.auth,
      this.storage,
      this.database,
      this.iam,
      this.extensions,
      this.cloudStorage,
      this.products,
    ];
    services.forEach(service => service.removeApiKey());
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