package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// manifestFile is the name of the signed manifest alef installs next to the
// component's executable.
const manifestFile = "component.json"

// providedContract is one entry of the manifest's `provides` list.
type providedContract struct {
	Contract         string `json:"contract"`
	InterfaceVersion uint32 `json:"interface_version"`
	ContractHash     string `json:"contract_hash"`
	Implementation   string `json:"implementation"`
}

// componentManifest is the part of alef's component.json the stdio handshake
// repeats.
type componentManifest struct {
	Identity struct {
		Component   string `json:"component"`
		Version     string `json:"version"`
		FeatureHash string `json:"feature_hash"`
	} `json:"identity"`
	Provides []providedContract `json:"provides"`
}

// stdioDescriptor is the JSON alef's host reads from the `describe` frame and
// compares with the verified manifest.
type stdioDescriptor struct {
	ProtocolVersion uint32             `json:"protocol_version"`
	ABIMajor        uint32             `json:"abi_major"`
	ABIMinor        uint32             `json:"abi_minor"`
	ComponentID     string             `json:"component_id"`
	ComponentVer    string             `json:"component_version"`
	FeatureSetHash  string             `json:"feature_set_hash"`
	Provides        []providedContract `json:"provides"`
}

// loadManifest reads component.json from the executable's directory. The host
// verifies that file (digest and signature) before it spawns the executable,
// so the identity it carries is the one the lock vouches for; the executable
// carries no copy of its own version or hashes, and so does not change when
// only those do.
func loadManifest() (componentManifest, error) {
	exe, err := os.Executable()
	if err != nil {
		return componentManifest{}, fmt.Errorf("locate executable: %w", err)
	}
	return readManifest(filepath.Join(filepath.Dir(exe), manifestFile))
}

func readManifest(path string) (componentManifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return componentManifest{}, fmt.Errorf("read manifest: %w", err)
	}
	var manifest componentManifest
	if err := json.Unmarshal(data, &manifest); err != nil {
		return componentManifest{}, fmt.Errorf("decode %s: %w", path, err)
	}
	return manifest, nil
}

func (m componentManifest) descriptor() stdioDescriptor {
	return stdioDescriptor{
		ProtocolVersion: 1,
		ABIMajor:        1,
		ABIMinor:        0,
		ComponentID:     m.Identity.Component,
		ComponentVer:    m.Identity.Version,
		FeatureSetHash:  m.Identity.FeatureHash,
		Provides:        m.Provides,
	}
}
