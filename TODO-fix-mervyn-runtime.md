# TODO: Fix mervyn not starting (runtime / image)

**Status:** Open — needs investigation.

**Symptom:** The mervyn service image or container does not come up as expected after deploy (see recent logs on the host: `gcloud compute ssh` → `cd ~/mervyn` → compose `logs` for service `mervyn`).

**Working hypothesis:** Conflict or mismatch between **Docker** and **Podman** on the target host (e.g. `docker` vs `docker-compose` vs `podman` / `podman-compose` / shims, different default socket or image store, or compose picking one runtime while a prior step used the other). That can show up as a loaded image not visible to the stack that runs `up`, or two competing stack definitions.

**Desired direction:** **Prefer Podman by default** for local `--local-build` and for on-VM ops (load + compose) so one runtime owns images and services end-to-end. Docker should remain a **clear fallback** only when Podman is not available, and the script/docs should make that split obvious to avoid “half Podman, half docker” on one machine.

**Next steps (checklist):**

- [ ] On the failing host: `command -v docker podman podman-compose docker-compose`; `docker context ls` / `podman system connection list`; see which `compose` actually runs and where images land (`docker images` vs `podman images` for `mervyn:deploy`).
- [ ] Reproduce with a single runtime (e.g. only Podman in `PATH` for a test) and compare.
- [ ] Re-read `scripts/deploy-gcp.sh` remote path: `load_prebuilt_image_if_local`, `compose_cmd`, and any `docker` / `docker-compose` that might run before Podman in edge cases; tighten order or add explicit `COMPOSE_` / runtime selection if needed.
- [ ] Document the “supported” GCE setup (Podman + podman-compose vs Docker-only) in one place after the fix is validated.

**Note:** `docker-compose.yml` service name is `mervyn` and image tag is `mervyn:deploy` when using pre-built deploy flow.
