// Stands in for `cloudflare:workers`: workerd constructs the default export
// as `new Entrypoint(ctx, env)` for each request.
export class WorkerEntrypoint {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
  }
}
