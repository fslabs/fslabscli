PHONY: build-artifacts
build-artifacts:
	nix flake show
	nix build .#release --fallback

publish: build-artifacts
	echo 'Publishing'
