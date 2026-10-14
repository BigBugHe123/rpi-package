# Open WebUI Computer with native rpi ACP

The test instance on bigfish-server is available at
`https://rpi-computer.laofu.online` (port 5232). It uses Computer 0.9.21 from the
unmodified official image through the domestic NJU mirror, pinned by digest. The existing
Open WebUI instance on port 5231 remains available.

```text
Computer workspace chat -> native ACP client -> rpi --acp -> rpi agent/tools
```

This does not install a Python Pipe Function or an OpenAI gateway. It reuses the
native Cline backend because its executable launch is exactly `command --acp`
with standard ACP authentication/session handling. No Cline executable is used.
The Pi backend was tested and rejected: its `get_available_models` RPC command
is not implemented by rpi's RPC transport.

## Agent profile

In Computer's Settings -> Admin -> Agents, configure:

```json
{
  "id": "rpi",
  "agent": "cline",
  "name": "RPI (native ACP)",
  "mode": "enabled",
  "command": "/opt/rpi/runtime/rpi",
  "models": ["deepseek/deepseek-v4-flash"],
  "default_model": "deepseek/deepseek-v4-flash"
}
```

The displayed model ID is `agent:rpi/deepseek/deepseek-v4-flash`. The rpi host
must load the `rpi-acp` library, have that provider/model configured,
and have writable session storage. The test deployment mounts an independent
project snapshot at `/workspace/rpi-package`, with ACP sessions in
`/data/rpi/acp-sessions`. Agent detection cannot discover rpi's model list through
the current ACP handshake, so the profile is explicitly enabled with one pinned
model. Switching to arbitrary models is not configured here.

## Original image deployment

[`deploy/docker-compose.yml`](deploy/docker-compose.yml) uses the official
Computer 0.9.21 image without modified Python modules. No client patches are
mounted. [`deploy/rpi-computer.caddy`](deploy/rpi-computer.caddy) configures the
domain. The service directory is `/home/bigfish/work/rpi-computer`, with
independent persistent state under `data/`. Initialize the administrator using
Computer's one-time setup link, disable signup, and add the profile above.

The runtime, provider config and extension libraries are ordinary host files
mounted read-only at `/opt/rpi`. They are not built into the Computer image.
The rpi process currently runs inside the container, where Computer launches
`/opt/rpi/runtime/rpi --acp` for each ACP connection.

## Running rpi outside the container

ACP here uses JSON-RPC over stdin/stdout, not a network listening port. A
separately started host process is therefore not discoverable through a URL.
To run rpi on the host while keeping Computer in Docker, use an executable
wrapper as the profile's `command` that forwards stdin/stdout through
`ssh -T` and starts the host's `rpi --acp`. The wrapper must forward the
arguments Computer supplies, keep stdout free of logs, and use noninteractive
SSH authentication. Both sides must see the workspace at the same absolute
path, because `session/new.cwd` is sent unchanged to rpi. Host-side rpi also
needs its own extension, provider credentials and writable session directory.

This forwarding setup is not currently deployed. The pinned original image
has no SSH client, so that client would also need to be supplied. Another
option is to run Computer directly on the host, alongside rpi.

## Validation and limitations

The earlier patched deployment passed complete tool/streaming/session-resume
checks. Those results do not describe the current unmodified client. Computer
0.9.21 has two known native ACP issues: a prompt can keep waiting after the
agent has finished, and `session/load` can replay old history into a new turn.
Removing the patches restores those upstream behaviors. The original image
can still initialize ACP sessions and receive streamed agent output.

The active original-image deployment was checked by comparing both native
client modules byte-for-byte with the pinned image, then verifying service
health, administrator login, project access and existing-chat persistence in
the browser. A native ACP session was created successfully. A real model reply
was received, but the test turn did not finish within 30 seconds and was
cancelled; the answer was then persisted. Complete original-client turn/resume
behavior is therefore not verified as working.

The test instance uses bypass for the rpi permission gate. Computer's native ACP
client does not negotiate the `pi-rust/questions` capability, so rpi extension
`ask_user` dialogs are not supported in this path. Permission requests in its
native backend are auto-approved or cancelled according to its approval mode;
this is not the same interactive behavior as the existing Open WebUI Pipe.

The `rpi-acp` extension provides ACP only. Computer is installed and configured
through its native administrator API; no UI launcher, installer, Python Pipe
or OpenAI gateway is included in the extension. Its runtime, provider config,
extensions, project and session data are all mounted from this service's own
directory, so the deployment does not depend on the older Open WebUI service.
