// Every scenario `run.mjs` knows, in the order it runs them. A new scenario
// is a module here exporting `{ name, shape, run(deployment) }` (and
// optionally `vars`, Worker vars for every version it deploys), added below.

import budgetRehearsal from "./budget-rehearsal.mjs";
import dynamicPath from "./dynamic-path.mjs";
import deploySmoke from "./deploy-smoke.mjs";
import gateOutage from "./gate-outage.mjs";
import healthBlockInit from "./health-block-init.mjs";
import healthDegraded from "./health-degraded.mjs";
import healthFailed from "./health-failed.mjs";
import healthStale from "./health-stale.mjs";
import healthStalePlan from "./health-stale-plan.mjs";
import permanentFault from "./permanent-fault.mjs";
import preparedImpresspressSite from "./prepared-impresspress-site.mjs";
import preparedWaferSite from "./prepared-wafer-site.mjs";
import stalePlan from "./stale-plan.mjs";

export default [
  preparedWaferSite,
  preparedImpresspressSite,
  gateOutage,
  permanentFault,
  dynamicPath,
  stalePlan,
  budgetRehearsal,
  healthDegraded,
  healthStale,
  healthStalePlan,
  healthFailed,
  healthBlockInit,
  deploySmoke,
];
