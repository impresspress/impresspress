# VectorMeter

Metered subscription combining a recurring platform row and graduated event-volume pricing in embedded Checkout. The public input is a safe estimate; actual billable usage must be reported server-to-server to Stripe.

The example estimates 350 blocks at USD 66.00/month and mounts a mocked embedded Checkout. Operators compare the estimate, provider meter, invoice, and entitlement states without treating the browser as authoritative.
