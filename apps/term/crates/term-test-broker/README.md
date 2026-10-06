# term-test-broker

Development fixture for the Term-to-Mix native session acceptance tests.
It includes the actual noded modules from their owning service and the actual
Term sealed-FD implementation from its owning core. It has no broker copy or
simulated session transport. The fixture controls an isolated broker runtime,
verified Unix ingress, restart and pause boundaries.
