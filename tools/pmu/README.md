# PMU acceptance

Run `bash tools/pmu/check.sh guest` inside the development container on an
x86-64 KVM host with a working virtual PMU and `/dev/kvm` mapped into the container.
Exit status `77` means that an acceptance check could not run.

For GitHub Actions, register isolated x86-64 KVM runners in the `pmu-isolated`
runner group with the `pmu` label.
In the group's access policy, allow only this repository and select
`OWNER/REPO/.github/workflows/test_pmu.yml@refs/heads/main` as the allowed workflow.
Protect the repository's `main` branch.
The runner-group policy applies even when a branch changes the workflow YAML.
Keep these runners separate from other workflow workloads.
