# Declarative identities and projections shared by every framework adapter.
let
  contract = builtins.fromJSON (builtins.readFile ./generated/gpu-contract.json);
  inherit (contract) vendors computeVendors;
  validRenderNode = node:
    builtins.isString node && builtins.match contract.renderNodePattern node != null;
  routes = gpu: let
    inventoryValid = builtins.all (vendor: vendor == null || builtins.elem vendor vendors) [(gpu.igpu or null) (gpu.dgpu or null)];
    media = gpu.media or null;
    render = gpu.render or null;
    main = gpu.dgpu or null;
    primary =
      if main != null
      then main
      else gpu.igpu or null;
    compute = gpu.compute or null;
    backend =
      if compute == null
      then null
      else compute.backend or null;
    matches = pattern: value: builtins.isString value && builtins.match pattern value != null;
    lower = builtins.replaceStrings ["A" "B" "C" "D" "E" "F"] ["a" "b" "c" "d" "e" "f"];
    # PCI addresses of the host's own render/media devices, never passed through.
    hostAddresses = map (route: lower (builtins.head (builtins.match "/dev/dri/by-path/pci-(.*)-render" route.renderNode))) (
      builtins.filter (route: route != null && validRenderNode (route.renderNode or null)) [render media]
    );
    passthrough = builtins.mapAttrs (name: entry: let
      projected = {
        inherit (entry) address id vendor;
        rom = entry.rom or "device";
        repeatable = entry.repeatable or false;
      };
    in
      if
        matches contract.passthroughNamePattern name
        && matches contract.pciAddressPattern (entry.address or null)
        && matches contract.pciIdPattern (entry.id or null)
        && builtins.elem (entry.vendor or null) vendors
        && builtins.elem projected.rom ["device" "vfct"]
        && builtins.isBool projected.repeatable
      then
        if builtins.elem entry.address hostAddresses
        then throw "GPU passthrough ${name}: ${entry.address} is the host's render or media device"
        else projected
      else throw "GPU passthrough ${name}: expected a lower-case PCI address and vendor:device ID, a known vendor and rom device or vfct") (gpu.passthrough or {});
    passthroughAddresses = map (entry: entry.address) (builtins.attrValues passthrough);
    passthroughDistinct = builtins.all (address: builtins.length (builtins.filter (other: other == address) passthroughAddresses) == 1) passthroughAddresses;
  in
    assert inventoryValid; {
      render =
        if render == null
        then {
          enable = false;
          renderNode = null;
        }
        else if validRenderNode (render.renderNode or null)
        then {
          enable = true;
          inherit (render) renderNode;
        }
        else throw "GPU render: expected a stable PCI render-node alias";
      media =
        if media == null
        then {
          enable = false;
          vendor = null;
          renderNode = null;
          libvaDriver = null;
        }
        else if
          builtins.elem (media.vendor or null) vendors
          && validRenderNode (media.renderNode or null)
          && builtins.isString (media.libvaDriver or null)
          && builtins.match ".*[^[:space:]].*" media.libvaDriver != null
          && builtins.elem media.vendor [(gpu.igpu or null) (gpu.dgpu or null)]
        then {
          enable = true;
          inherit (media) vendor renderNode libvaDriver;
        }
        else throw "GPU media: expected an inventoried vendor, stable render node and libva driver";
      compute =
        if compute == null
        then null
        else if backend == null || !(builtins.hasAttr backend computeVendors)
        then throw "GPU compute: expected backend oneapi, rocm, or cuda"
        else if primary != computeVendors.${backend}
        then throw "GPU compute: ${backend} requires a ${computeVendors.${backend}} primary GPU (dGPU, otherwise iGPU)"
        else {inherit backend;};
      passthrough =
        if passthroughDistinct
        then passthrough
        else throw "GPU passthrough: each PCI address may be declared once";
    };
  normalize = gpuData: let
    igpu = gpuData.igpu or null;
    dgpu = gpuData.dgpu or null;
    main =
      if dgpu != null
      then dgpu
      else igpu;
    has = vendor: igpu == vendor || dgpu == vendor;
    selected = routes gpuData;
  in
    builtins.seq selected {
      inherit igpu dgpu main;
      deviceType = gpuData.deviceType or null;
      inherit (selected) compute passthrough;
      render =
        if selected.render.enable
        then {inherit (selected.render) renderNode;}
        else null;
      routes = selected;
      isHybrid = igpu != null && dgpu != null;
      vendors = builtins.filter has vendors;
      has = {
        amd = has "amd";
        intel = has "intel";
        nvidia = has "nvidia";
      };
      mainGpu = main;
      hasAmd = has "amd";
      hasIntel = has "intel";
      hasNvidia = has "nvidia";
    };
  hostRecord = host: (host.gpu or {}) // {deviceType = host.deviceType or null;};
in {
  inherit normalize vendors validRenderNode routes contract;
  noGpu = normalize {};
  forHost = hosts: name: normalize (hostRecord (hosts.${name} or {}));
  forHosts = hosts: builtins.mapAttrs (_: host: normalize (hostRecord host)) hosts;
}
