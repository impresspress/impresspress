// Every scenario `run.mjs` knows, in the order it runs them. A new scenario
// is a module here exporting `{ name, shape, run(deployment) }` (and
// optionally `vars`, Worker vars for every version it deploys), added below.

import budgetRehearsal from "./budget-rehearsal.mjs";
import dynamicPath from "./dynamic-path.mjs";
import permanentFault from "./permanent-fault.mjs";
import preparedImpresspressSite from "./prepared-impresspress-site.mjs";
import preparedWaferSite from "./prepared-wafer-site.mjs";
import stalePlan from "./stale-plan.mjs";

export default [
  preparedWaferSite,
  preparedImpresspressSite,
  permanentFault,
  dynamicPath,
  stalePlan,
  budgetRehearsal,
];
