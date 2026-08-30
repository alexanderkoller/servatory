# Wi-Fi dashboard and notifications

Servatory uses the StickS3's own network interface. The Linux daemon still
collects the measurements, but it neither serves the dashboard nor contacts
ntfy. This separation keeps the network interface responsive when the host
hangs. During an outage, host measurements are necessarily last-known values;
the stick marks them stale and creates its own `HOST CONNECTION LOST` incident.

## Provision the stick

On the first boot, the display shows `WIFI SETUP`, a temporary network name,
and the address `192.168.4.1`. Connect a phone or computer to that open network,
open `http://192.168.4.1/`, and enter:

- the normal Wi-Fi network and password;
- the device hostname, normally `servatory` (up to 32 characters);
- the generated ntfy topic, or a replacement of your choice.

The setup network is intentionally temporary and has no password. It provides
DHCP, so the client needs no manual IP configuration. Saving the form writes
the settings to the final flash sector, shuts down the open setup network, and
restarts the device in station mode. Hold the front button while powering on to
return to provisioning and replace the stored values.

Servatory generates a new random ntfy topic when provisioning begins and
prefills it in the form. Use **Copy topic** to copy it into the ntfy app before
saving. The field remains editable if you prefer your own long, unguessable
topic.

The ntfy topic acts as a secret on the public ntfy.sh service. Subscribe to the
same topic in the iPhone app, do not reuse it as a descriptive public name, and
avoid placing sensitive infrastructure details in health messages.

## Open the dashboard

The default dashboard is available at `http://servatory/` when the router
registers DHCP hostnames in its local DNS. Servatory sends the hostname selected
during provisioning as DHCP option 12; clients normally expand the bare name
using the router-provided DNS search domain. For example, a Fritz!Box may resolve
`servatory` through `servatory.fritz.box`.

The stick also advertises `http://servatory.local/` using multicast DNS. This is
the infrastructure-independent fallback on the same network link. If the router
does not register DHCP hostnames and the network filters multicast DNS, use the
IP address assigned to the stick by the router instead.

The responsive dashboard follows the view order under `outputs.http.views`.
It presents the same information as the LCD, but combines paginated disks and
guests into continuous sections and uses compact utilization bars for resources.
An About card shows the firmware, daemon, protocol, hostname, and the stick's
current DHCP address. A Notifications card shows the configured ntfy server and
topic, provides a copy button, can send a test notification, and can generate a
new random topic. Topic generation writes the replacement to flash immediately
and switches subsequent notifications without Wi-Fi reprovisioning. Existing
ntfy subscriptions must be changed to the newly displayed topic. The page also
shows whether the feed is live, its age, and the complete active-incident list.
It refreshes every five seconds and does not load scripts, fonts, libraries, or
other assets from the Internet.

When the HTTP server is enabled and DHCP has assigned an address, the Stick's
About screen shows a QR code for the dashboard root URL. The adjacent LCD text
uses short semantic versions to leave the code a reliable three display pixels
per module; the web About card retains the complete build identifiers. If HTTP
is disabled or no address is available, the LCD uses the detailed text-only
About screen instead.

The stick also serves four machine-readable routes:

- `GET /api/v1/health` returns current incident and summary data as JSON;
- `GET /api/v1/device` reports firmware, Wi-Fi, and snapshot-age information;
- `POST /api/v1/notifications/test` queues a test ntfy message;
- `GET /healthz` checks the stick's HTTP service;
- `GET /` returns the dashboard.

## Understand notification behavior

Servatory compares incident identities rather than rendered messages. A metric
changing from `CPU 85%` to `CPU 86%` therefore does not create another alert.
The stick does send when an incident starts, escalates from warning to critical,
recovers, or remains critical for the configured repeat interval. Independent
simultaneous incidents produce independent notifications.

Failed deliveries enter an eight-message queue. When the queue is full, the
oldest pending item is discarded so newer state is retained. HTTPS delivery to
ntfy.sh validates the server certificate against the bundled ISRG Root X1
certificate. A custom HTTPS server must use a chain rooted there; otherwise use
an HTTP endpoint only on a network where unencrypted health messages are
acceptable.

The `urgent` ntfy priority is ntfy's highest priority. It is not Apple's
specially entitled Critical Alerts mechanism, so iPhone Focus and notification
settings can still suppress it.

## Failure boundaries

Wi-Fi availability depends on power. If the host supplies the stick's only USB
power and that power disappears, the internal battery determines how long the
dashboard and notifications remain available. An independent USB supply avoids
that dependency.

The dashboard has no history database. A restart clears the last health
snapshot and reports the host feed as missing until another valid USB update
arrives. Credentials and the output manifest persist; frequent health snapshots
remain in RAM to avoid flash wear.
