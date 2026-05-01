# Probe-Exquis-USB
#
# Standalone diagnostic: enumerates everything Windows knows about connected
# Exquis devices and prints, for each one:
#   - the real USB instance ID (with the firmware-provided USB serial)
#   - VID/PID, manufacturer string, location info
#   - every child PnP node, including MIDI ports and Windows MIDI Service
#     (SWD\MIDISRV\MIDIU_KSA_*) endpoints
#
# Goal: see whether the real USB serials are stable across power cycles, and
# whether we can use the parent-walk to bind a MIDI port to its physical
# Exquis without relying on Windows MIDI Service synthetic IDs.
#
# Run twice, with a full power-cycle of all four boards in between, and
# compare. Real USB serials should be identical in both runs; MIDIU_KSA_*
# hashes likely will not.

$ErrorActionPreference = 'Stop'

function Get-PropOrNull {
    param([string]$InstanceId, [string]$KeyName)
    try {
        $p = Get-PnpDeviceProperty -InstanceId $InstanceId -KeyName $KeyName -ErrorAction Stop
        if ($null -ne $p -and $null -ne $p.Data) { return [string]$p.Data }
    } catch { }
    return $null
}

function Get-Children {
    param([string]$InstanceId)
    try {
        $p = Get-PnpDeviceProperty -InstanceId $InstanceId -KeyName 'DEVPKEY_Device_Children' -ErrorAction Stop
        if ($null -eq $p -or $null -eq $p.Data) { return @() }
        $data = $p.Data
        if ($data -is [string]) { return @($data) }
        return @($data)
    } catch { return @() }
}

function Parse-VidPid {
    param([string]$InstanceId)
    # Note: $PID is a reserved automatic variable in PowerShell (current
    # process ID), so we use $prodId here.
    $vid = $null; $prodId = $null
    if ($InstanceId -match 'VID_([0-9A-Fa-f]{4})') { $vid = $matches[1].ToUpper() }
    if ($InstanceId -match 'PID_([0-9A-Fa-f]{4})') { $prodId = $matches[1].ToUpper() }
    return @{ Vid = $vid; Pid = $prodId }
}

function Parse-UsbSerial {
    param([string]$InstanceId)
    if ($InstanceId -match '^USB\\VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}\\([^\\]+)$') {
        return $matches[1]
    }
    return $null
}

# 1. Find every PnP node that references "Exquis" or "Intuitive".
#    Win32_PnPEntity exposes Name + Manufacturer directly (no per-device
#    API roundtrip), so this filter is fast.
Write-Host '== Stage 1: candidate PnP nodes (Win32_PnPEntity match) ==' -ForegroundColor Cyan
$candidates = Get-CimInstance -ClassName Win32_PnPEntity |
    Where-Object {
        $_.Name -match 'Exquis|Intuitive' -or
        $_.Manufacturer -match 'Intuitive|Exquis' -or
        $_.PNPDeviceID -match 'Exquis|Intuitive'
    }

if (-not $candidates) {
    Write-Host '  (no matches — are the boards powered on?)' -ForegroundColor Yellow
    return
}

$candidates | Sort-Object PNPDeviceID | ForEach-Object {
    "{0,-12} {1,-70} mfg={2}" -f $_.PNPClass, $_.PNPDeviceID, $_.Manufacturer | Write-Host
}

# 2. Isolate the real USB composite-device parents and walk parents of
#    candidates that live under USB\ to capture any we missed.
Write-Host ''
Write-Host '== Stage 2: real USB parents (these carry the firmware serial) ==' -ForegroundColor Cyan
$parentSet = New-Object System.Collections.Generic.HashSet[string]
foreach ($c in $candidates) {
    $id = $c.PNPDeviceID
    if ($id -match '^USB\\VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}\\[^\\]+$') {
        [void]$parentSet.Add($id)
        continue
    }
    if ($id -notmatch '^USB\\') { continue }
    $cur = $id
    while ($true) {
        $parentId = Get-PropOrNull -InstanceId $cur -KeyName 'DEVPKEY_Device_Parent'
        if (-not $parentId) { break }
        if ($parentId -match '^USB\\VID_[0-9A-Fa-f]{4}&PID_[0-9A-Fa-f]{4}\\[^\\]+$') {
            [void]$parentSet.Add($parentId)
            break
        }
        if ($parentId -notmatch '^USB\\') { break }
        $cur = $parentId
    }
}

if ($parentSet.Count -eq 0) {
    Write-Host '  (no real USB parents found — the fallback path can not get a real serial here)' -ForegroundColor Yellow
    Write-Host ''
    Write-Host '== Done ==' -ForegroundColor Cyan
    return
}

foreach ($parentInst in ($parentSet | Sort-Object)) {
    $vp = Parse-VidPid $parentInst
    $serial = Parse-UsbSerial $parentInst
    $mfg   = Get-PropOrNull -InstanceId $parentInst -KeyName 'DEVPKEY_Device_Manufacturer'
    $fname = Get-PropOrNull -InstanceId $parentInst -KeyName 'DEVPKEY_NAME'
    $loc   = Get-PropOrNull -InstanceId $parentInst -KeyName 'DEVPKEY_Device_LocationInfo'
    Write-Host ''
    Write-Host "  USB parent: $parentInst" -ForegroundColor Green
    Write-Host "    VID:PID    = $($vp.Vid):$($vp.Pid)"
    Write-Host "    serial     = $serial"
    Write-Host "    name       = $fname"
    Write-Host "    mfg        = $mfg"
    Write-Host "    location   = $loc"

    # Recursive walk of children to find MIDI endpoints.
    $stack = New-Object System.Collections.Stack
    foreach ($child in (Get-Children $parentInst)) {
        $stack.Push(@{ Id = $child; Depth = 1 })
    }
    $midisrvHits = @()
    while ($stack.Count -gt 0) {
        $node = $stack.Pop()
        $id = $node.Id
        $depth = $node.Depth
        $name = Get-PropOrNull -InstanceId $id -KeyName 'DEVPKEY_NAME'
        $svc  = Get-PropOrNull -InstanceId $id -KeyName 'DEVPKEY_Device_Service'
        "    {0}child[d{1}] {2}  (svc={3}, name={4})" -f ('  ' * $depth), $depth, $id, $svc, $name | Write-Host
        if ($id -match 'MIDISRV\\MIDIU_') { $midisrvHits += $id }
        foreach ($c2 in (Get-Children $id)) {
            $stack.Push(@{ Id = $c2; Depth = $depth + 1 })
        }
    }

    if ($midisrvHits) {
        Write-Host "    MIDISRV synthetic IDs under this device:" -ForegroundColor Yellow
        $midisrvHits | ForEach-Object { Write-Host "      $_" }
    }
}

Write-Host ''
Write-Host '== Summary: real USB serials (this is what matters) ==' -ForegroundColor Cyan
foreach ($parentInst in ($parentSet | Sort-Object)) {
    $vp = Parse-VidPid $parentInst
    $serial = Parse-UsbSerial $parentInst
    "  VID={0} PID={1} serial={2}" -f $vp.Vid, $vp.Pid, $serial | Write-Host
}

# 3. Build a KSA-hash → USB-serial map. Each USB parent has exactly one
#    SWD\MIDISRV\MIDIU_KSA_<hash> child; that hash also appears in the
#    SWD\MMDEVAPI\MIDIU_KSA_<hash>_<n>_<m> grandchildren which are the
#    actual winmm/midir endpoints.
Write-Host ''
Write-Host '== Stage 3: KSA hash -> real USB serial (built from PnP tree) ==' -ForegroundColor Cyan
$ksaToSerial = @{}
foreach ($parentInst in $parentSet) {
    $serial = Parse-UsbSerial $parentInst
    foreach ($child in (Get-Children $parentInst)) {
        if ($child -match 'MIDISRV\\MIDIU_KSA_(\d+)') {
            $ksaToSerial[$matches[1]] = $serial
        }
        # Also walk one level deeper for MMDEVAPI grandchildren (some Windows
        # versions register MMDEVAPI as direct child of USB; harmless either way).
        foreach ($gchild in (Get-Children $child)) {
            if ($gchild -match 'MMDEVAPI\\MIDIU_KSA_(\d+)_') {
                if (-not $ksaToSerial.ContainsKey($matches[1])) {
                    $ksaToSerial[$matches[1]] = $serial
                }
            }
        }
    }
}
foreach ($kv in ($ksaToSerial.GetEnumerator() | Sort-Object Name)) {
    "  KSA {0,-22} -> serial {1}" -f $kv.Name, $kv.Value | Write-Host
}

# 4. Enumerate MIDI ports the way midir/winmm see them, via the winmm
#    P/Invoke. Each port name will just be "Exquis" (no per-device tag),
#    but the count and the order are exactly what midir gets.
Write-Host ''
Write-Host '== Stage 4: midir/winmm port enumeration (this is what xentool gets) ==' -ForegroundColor Cyan

$winmmCode = @'
using System;
using System.Runtime.InteropServices;

public static class WinMM {
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Auto)]
    public struct MIDIINCAPS {
        public ushort wMid;
        public ushort wPid;
        public uint vDriverVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)]
        public string szPname;
        public uint dwSupport;
    }

    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Auto)]
    public struct MIDIOUTCAPS {
        public ushort wMid;
        public ushort wPid;
        public uint vDriverVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)]
        public string szPname;
        public ushort wTechnology;
        public ushort wVoices;
        public ushort wNotes;
        public ushort wChannelMask;
        public uint dwSupport;
    }

    [DllImport("winmm.dll")]
    public static extern uint midiInGetNumDevs();

    [DllImport("winmm.dll", CharSet=CharSet.Auto, EntryPoint="midiInGetDevCapsW")]
    public static extern uint midiInGetDevCaps(IntPtr uDeviceID, ref MIDIINCAPS lpCaps, uint cbMidiInCaps);

    [DllImport("winmm.dll")]
    public static extern uint midiOutGetNumDevs();

    [DllImport("winmm.dll", CharSet=CharSet.Auto, EntryPoint="midiOutGetDevCapsW")]
    public static extern uint midiOutGetDevCaps(IntPtr uDeviceID, ref MIDIOUTCAPS lpCaps, uint cbMidiOutCaps);
}
'@
try {
    Add-Type -TypeDefinition $winmmCode -Language CSharp -ErrorAction Stop
} catch {
    # Ignore "already added" on subsequent runs in same session.
}

$inN  = [WinMM]::midiInGetNumDevs()
$outN = [WinMM]::midiOutGetNumDevs()
Write-Host "  winmm reports $inN MIDI inputs, $outN MIDI outputs"

$inCapsSize  = [System.Runtime.InteropServices.Marshal]::SizeOf([type][WinMM+MIDIINCAPS])
$outCapsSize = [System.Runtime.InteropServices.Marshal]::SizeOf([type][WinMM+MIDIOUTCAPS])

$winmmInputs = @()
for ($i = 0; $i -lt $inN; $i++) {
    $caps = New-Object 'WinMM+MIDIINCAPS'
    $rc = [WinMM]::midiInGetDevCaps([IntPtr]$i, [ref]$caps, [uint32]$inCapsSize)
    if ($rc -eq 0) {
        $exquis = $caps.szPname -match 'Exquis'
        $winmmInputs += [PSCustomObject]@{
            Index   = $i
            Name    = $caps.szPname
            Mid     = $caps.wMid
            Pid     = $caps.wPid
            Exquis  = $exquis
        }
        $marker = if ($exquis) { '[Exquis]' } else { '' }
        "  in[{0}]  name='{1}' mid={2} pid={3} {4}" -f $i, $caps.szPname, $caps.wMid, $caps.wPid, $marker | Write-Host
    } else {
        "  in[{0}] <error rc={1}>" -f $i, $rc | Write-Host
    }
}
Write-Host ''
$winmmOutputs = @()
for ($i = 0; $i -lt $outN; $i++) {
    $caps = New-Object 'WinMM+MIDIOUTCAPS'
    $rc = [WinMM]::midiOutGetDevCaps([IntPtr]$i, [ref]$caps, [uint32]$outCapsSize)
    if ($rc -eq 0) {
        $exquis = $caps.szPname -match 'Exquis'
        $winmmOutputs += [PSCustomObject]@{
            Index  = $i
            Name   = $caps.szPname
            Mid    = $caps.wMid
            Pid    = $caps.wPid
            Exquis = $exquis
        }
        $marker = if ($exquis) { '[Exquis]' } else { '' }
        "  out[{0}] name='{1}' mid={2} pid={3} {4}" -f $i, $caps.szPname, $caps.wMid, $caps.wPid, $marker | Write-Host
    } else {
        "  out[{0}] <error rc={1}>" -f $i, $rc | Write-Host
    }
}

# 5. List MMDEVAPI _0_0 (input) and _1_0 (output) entries in the order they
#    come back from Win32_PnPEntity. Each carries a KSA hash that we mapped
#    to a real USB serial in Stage 3, so this gives us the bridge candidate.
Write-Host ''
Write-Host '== Stage 5: MMDEVAPI endpoints -> KSA -> real USB serial ==' -ForegroundColor Cyan
$mmdevs = Get-CimInstance -ClassName Win32_PnPEntity |
    Where-Object { $_.PNPDeviceID -match '^SWD\\MMDEVAPI\\MIDIU_KSA_' }

# Pull KSA hash and (suffix _N_M) endpoint role from each InstanceID.
# Empirically: _0_0 = input direction, _1_0 = output (we'll verify by
# matching counts against winmm's input/output totals).
$mmInputs  = @()
$mmOutputs = @()
foreach ($e in $mmdevs) {
    if ($e.PNPDeviceID -match 'MIDIU_KSA_(\d+)_(\d+)_(\d+)$') {
        $hash = $matches[1]
        $role = "$($matches[2])_$($matches[3])"
        $serial = $ksaToSerial[$hash]
        $row = [PSCustomObject]@{
            InstanceId = $e.PNPDeviceID
            Hash       = $hash
            Role       = $role
            Serial     = $serial
            Name       = $e.Name
        }
        if ($role -eq '0_0') { $mmInputs  += $row }
        elseif ($role -eq '1_0') { $mmOutputs += $row }
    }
}

Write-Host "  MMDEVAPI _0_0 (candidate inputs, in PnP enumeration order):"
$idx = 0
foreach ($r in $mmInputs) {
    "    [{0}] KSA {1,-22} -> serial {2}  ({3})" -f $idx, $r.Hash, $r.Serial, $r.InstanceId | Write-Host
    $idx++
}
Write-Host ''
Write-Host "  MMDEVAPI _1_0 (candidate outputs, in PnP enumeration order):"
$idx = 0
foreach ($r in $mmOutputs) {
    "    [{0}] KSA {1,-22} -> serial {2}  ({3})" -f $idx, $r.Hash, $r.Serial, $r.InstanceId | Write-Host
    $idx++
}

# 6. Bridge check. If winmm's count of Exquis ports matches the count of
#    MMDEVAPI entries, the index-to-serial mapping is well-defined IF the
#    two enumerations are in the same order. Whether they are is an
#    empirical question — running this twice across a power cycle, with
#    boards plugged into different orders, tells us if the orders track.
Write-Host ''
Write-Host '== Stage 6: bridge candidate (midir port index -> real serial) ==' -ForegroundColor Cyan
$exquisIns  = @($winmmInputs  | Where-Object { $_.Exquis })
$exquisOuts = @($winmmOutputs | Where-Object { $_.Exquis })
"  winmm Exquis inputs:  $($exquisIns.Count)" | Write-Host
"  MMDEVAPI _0_0 entries: $($mmInputs.Count)" | Write-Host
"  winmm Exquis outputs: $($exquisOuts.Count)" | Write-Host
"  MMDEVAPI _1_0 entries: $($mmOutputs.Count)" | Write-Host

if ($exquisIns.Count -eq $mmInputs.Count -and $exquisIns.Count -gt 0) {
    Write-Host ''
    Write-Host '  Hypothesised input bridge (assumes both lists are in the same order):' -ForegroundColor Green
    for ($i = 0; $i -lt $exquisIns.Count; $i++) {
        "    midir-in[{0}] -> serial {1}  (KSA {2})" -f `
            $exquisIns[$i].Index, $mmInputs[$i].Serial, $mmInputs[$i].Hash | Write-Host
    }
} else {
    Write-Host '  (counts do not match — bridge cannot be confirmed without a richer API)' -ForegroundColor Yellow
}

# 7. WinRT MIDI enumeration. WinRT exposes each MIDI port's
#    DeviceInformation.Id, which directly contains the SWD\MMDEVAPI\...
#    path (and therefore the KSA hash). This is purely for identification
#    — we do NOT propose switching xentool's runtime backend. We only need
#    to know whether WinRT can give us the (port-ordering, KSA-id) pair so
#    a one-shot lookup at startup can pair midir's ports to real USB
#    serials.
Write-Host ''
Write-Host '== Stage 7: WinRT MIDI enumeration (identification only) ==' -ForegroundColor Cyan

function Get-WinRTMidiPorts {
    try {
        Add-Type -AssemblyName System.Runtime.WindowsRuntime -ErrorAction Stop
    } catch {
        Write-Host "  WinRT support not available: $($_.Exception.Message)" -ForegroundColor Yellow
        return $null
    }

    try {
        # Force-load the WinRT type projections.
        $null = [Windows.Devices.Midi.MidiInPort, Windows.Devices.Midi, ContentType = WindowsRuntime]
        $null = [Windows.Devices.Midi.MidiOutPort, Windows.Devices.Midi, ContentType = WindowsRuntime]
        $null = [Windows.Devices.Enumeration.DeviceInformation, Windows.Devices.Enumeration, ContentType = WindowsRuntime]
    } catch {
        Write-Host "  Failed to load WinRT types: $($_.Exception.Message)" -ForegroundColor Yellow
        return $null
    }

    # Find the generic AsTask<TResult>(IAsyncOperation<TResult>) overload.
    $asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() |
        Where-Object {
            $_.Name -eq 'AsTask' -and
            $_.IsGenericMethod -and
            $_.GetGenericArguments().Length -eq 1 -and
            $_.GetParameters().Count -eq 1
        } | Select-Object -First 1
    if (-not $asTask) {
        Write-Host "  Could not locate WindowsRuntimeSystemExtensions.AsTask<T>" -ForegroundColor Yellow
        return $null
    }

    $awaitOp = {
        param($asyncOp, $resultType)
        $netTask = $asTask.MakeGenericMethod($resultType).Invoke($null, @($asyncOp))
        $netTask.Wait(-1) | Out-Null
        return $netTask.Result
    }

    $resultType = [Windows.Devices.Enumeration.DeviceInformationCollection]

    $inSel = [Windows.Devices.Midi.MidiInPort]::GetDeviceSelector()
    $inAsync = [Windows.Devices.Enumeration.DeviceInformation]::FindAllAsync($inSel)
    $inDevs = & $awaitOp $inAsync $resultType

    $outSel = [Windows.Devices.Midi.MidiOutPort]::GetDeviceSelector()
    $outAsync = [Windows.Devices.Enumeration.DeviceInformation]::FindAllAsync($outSel)
    $outDevs = & $awaitOp $outAsync $resultType

    return @{ In = $inDevs; Out = $outDevs }
}

$winrt = Get-WinRTMidiPorts
if (-not $winrt) {
    Write-Host "  (skipping bridge analysis)" -ForegroundColor Yellow
} else {
    function Extract-Ksa { param([string]$id)
        if ($id -match 'MIDIU_KSA_(\d+)') { return $matches[1] }
        return $null
    }

    Write-Host "  WinRT input ports (in WinRT enumeration order):"
    $i = 0
    $winrtIns = @()
    foreach ($d in $winrt.In) {
        $ksa = Extract-Ksa $d.Id
        $serial = if ($ksa) { $ksaToSerial[$ksa] } else { $null }
        $winrtIns += [PSCustomObject]@{
            Index = $i; Name = $d.Name; Id = $d.Id; Ksa = $ksa; Serial = $serial
        }
        "    [{0}] name='{1}'" -f $i, $d.Name | Write-Host
        "         id='{0}'" -f $d.Id | Write-Host
        "         KSA={0}  serial={1}" -f $ksa, $serial | Write-Host
        $i++
    }
    Write-Host ''
    Write-Host "  WinRT output ports (in WinRT enumeration order):"
    $i = 0
    $winrtOuts = @()
    foreach ($d in $winrt.Out) {
        $ksa = Extract-Ksa $d.Id
        $serial = if ($ksa) { $ksaToSerial[$ksa] } else { $null }
        $winrtOuts += [PSCustomObject]@{
            Index = $i; Name = $d.Name; Id = $d.Id; Ksa = $ksa; Serial = $serial
        }
        "    [{0}] name='{1}'" -f $i, $d.Name | Write-Host
        "         id='{0}'" -f $d.Id | Write-Host
        "         KSA={0}  serial={1}" -f $ksa, $serial | Write-Host
        $i++
    }

    # 8. Bridge: line up winmm/midir order against WinRT order. midir on
    #    Windows uses winmm. If both lists agree on the order of "Exquis"
    #    ports, then a one-shot WinRT enumeration at xentool startup gives
    #    us the (midir_port_index -> real_serial) map for free, with no
    #    runtime-backend change.
    Write-Host ''
    Write-Host '== Stage 8: midir/winmm <-> WinRT bridge ==' -ForegroundColor Cyan

    $exquisInsWinMM   = @($winmmInputs  | Where-Object { $_.Exquis })
    $exquisOutsWinMM  = @($winmmOutputs | Where-Object { $_.Exquis })
    $exquisInsWinRT   = @($winrtIns  | Where-Object { $_.Name -match 'Exquis' })
    $exquisOutsWinRT  = @($winrtOuts | Where-Object { $_.Name -match 'Exquis' })

    "  winmm Exquis inputs:  $($exquisInsWinMM.Count)" | Write-Host
    "  WinRT Exquis inputs:  $($exquisInsWinRT.Count)" | Write-Host
    "  winmm Exquis outputs: $($exquisOutsWinMM.Count)" | Write-Host
    "  WinRT Exquis outputs: $($exquisOutsWinRT.Count)" | Write-Host

    if ($exquisInsWinMM.Count -eq $exquisInsWinRT.Count -and $exquisInsWinMM.Count -gt 0) {
        Write-Host ''
        Write-Host '  Hypothesised input bridge (winmm[i] = WinRT[i]):' -ForegroundColor Green
        for ($i = 0; $i -lt $exquisInsWinMM.Count; $i++) {
            $wm = $exquisInsWinMM[$i]
            $wr = $exquisInsWinRT[$i]
            "    midir-in[{0}] (winmm idx {1}) -> serial {2}  (KSA {3})" -f `
                $i, $wm.Index, $wr.Serial, $wr.Ksa | Write-Host
        }
    } else {
        Write-Host '  Input counts disagree — cannot bridge by index.' -ForegroundColor Yellow
    }

    if ($exquisOutsWinMM.Count -eq $exquisOutsWinRT.Count -and $exquisOutsWinMM.Count -gt 0) {
        Write-Host ''
        Write-Host '  Hypothesised output bridge (winmm[i] = WinRT[i]):' -ForegroundColor Green
        for ($i = 0; $i -lt $exquisOutsWinMM.Count; $i++) {
            $wm = $exquisOutsWinMM[$i]
            $wr = $exquisOutsWinRT[$i]
            "    midir-out[{0}] (winmm idx {1}) -> serial {2}  (KSA {3})" -f `
                $i, $wm.Index, $wr.Serial, $wr.Ksa | Write-Host
        }
    } else {
        Write-Host '  Output counts disagree — cannot bridge by index.' -ForegroundColor Yellow
    }
}

# 9. DRV_QUERYDEVICEINTERFACE — given a winmm device index (the same
#    index midir uses), the OS hands back the underlying device interface
#    path. That string is `\\?\SWD#MMDEVAPI#MIDIU_KSA_<hash>_n_m#<guid>`
#    — exactly the form we need to extract the KSA hash, which we already
#    mapped to a real USB serial in Stage 3. This is the deterministic
#    bridge with no WinRT, no GUID matching, no guesswork.
Write-Host ''
Write-Host '== Stage 9: DRV_QUERYDEVICEINTERFACE (the actual bridge) ==' -ForegroundColor Cyan

$drvCode = @'
using System;
using System.Runtime.InteropServices;

public static class WinMMDrv {
    public const uint DRV_QUERYDEVICEINTERFACE     = 0x080C;
    public const uint DRV_QUERYDEVICEINTERFACESIZE = 0x080D;
    public const uint CALLBACK_NULL = 0;
    public const uint MMSYSERR_NOERROR = 0;

    [DllImport("winmm.dll")]
    public static extern uint midiInOpen(out IntPtr lphMidiIn, uint uDeviceID, IntPtr dwCallback, IntPtr dwInstance, uint dwFlags);
    [DllImport("winmm.dll")]
    public static extern uint midiInClose(IntPtr hMidiIn);
    [DllImport("winmm.dll")]
    public static extern uint midiInMessage(IntPtr hMidiIn, uint uMsg, IntPtr dwParam1, IntPtr dwParam2);

    [DllImport("winmm.dll")]
    public static extern uint midiOutOpen(out IntPtr lphMidiOut, uint uDeviceID, IntPtr dwCallback, IntPtr dwInstance, uint dwFlags);
    [DllImport("winmm.dll")]
    public static extern uint midiOutClose(IntPtr hMidiOut);
    [DllImport("winmm.dll")]
    public static extern uint midiOutMessage(IntPtr hMidiOut, uint uMsg, IntPtr dwParam1, IntPtr dwParam2);
}
'@
try { Add-Type -TypeDefinition $drvCode -Language CSharp -ErrorAction Stop } catch { }

# Try DRV_QUERYDEVICEINTERFACE two ways: first against the device-id-as-handle
# (cheap, no exclusive open), then by actually opening the port if that fails.
function Query-MidiDeviceInterface {
    param([uint32]$DeviceId, [bool]$IsInput)

    $sizePtr = [System.Runtime.InteropServices.Marshal]::AllocHGlobal(4)
    [System.Runtime.InteropServices.Marshal]::WriteInt32($sizePtr, 0)
    try {
        $rc = if ($IsInput) {
            [WinMMDrv]::midiInMessage([IntPtr]$DeviceId, [WinMMDrv]::DRV_QUERYDEVICEINTERFACESIZE, $sizePtr, [IntPtr]::Zero)
        } else {
            [WinMMDrv]::midiOutMessage([IntPtr]$DeviceId, [WinMMDrv]::DRV_QUERYDEVICEINTERFACESIZE, $sizePtr, [IntPtr]::Zero)
        }
        $cbSize = [System.Runtime.InteropServices.Marshal]::ReadInt32($sizePtr)
        if ($rc -ne 0 -or $cbSize -le 0) {
            # Device-id-as-handle didn't work; fall back to a real open.
            $h = [IntPtr]::Zero
            $openRc = if ($IsInput) {
                [WinMMDrv]::midiInOpen([ref]$h, $DeviceId, [IntPtr]::Zero, [IntPtr]::Zero, [WinMMDrv]::CALLBACK_NULL)
            } else {
                [WinMMDrv]::midiOutOpen([ref]$h, $DeviceId, [IntPtr]::Zero, [IntPtr]::Zero, [WinMMDrv]::CALLBACK_NULL)
            }
            if ($openRc -ne 0) {
                return @{ Ok = $false; Reason = "open failed (rc=$openRc), query-as-id rc=$rc"; Path = $null }
            }
            try {
                [System.Runtime.InteropServices.Marshal]::WriteInt32($sizePtr, 0)
                $rc = if ($IsInput) {
                    [WinMMDrv]::midiInMessage($h, [WinMMDrv]::DRV_QUERYDEVICEINTERFACESIZE, $sizePtr, [IntPtr]::Zero)
                } else {
                    [WinMMDrv]::midiOutMessage($h, [WinMMDrv]::DRV_QUERYDEVICEINTERFACESIZE, $sizePtr, [IntPtr]::Zero)
                }
                $cbSize = [System.Runtime.InteropServices.Marshal]::ReadInt32($sizePtr)
                if ($rc -ne 0 -or $cbSize -le 0) {
                    return @{ Ok = $false; Reason = "size query failed (rc=$rc, cb=$cbSize)"; Path = $null }
                }
                $buf = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($cbSize)
                try {
                    $rc = if ($IsInput) {
                        [WinMMDrv]::midiInMessage($h, [WinMMDrv]::DRV_QUERYDEVICEINTERFACE, $buf, [IntPtr]$cbSize)
                    } else {
                        [WinMMDrv]::midiOutMessage($h, [WinMMDrv]::DRV_QUERYDEVICEINTERFACE, $buf, [IntPtr]$cbSize)
                    }
                    if ($rc -ne 0) {
                        return @{ Ok = $false; Reason = "interface query failed (rc=$rc)"; Path = $null }
                    }
                    $path = [System.Runtime.InteropServices.Marshal]::PtrToStringUni($buf, [int]($cbSize / 2)).TrimEnd([char]0)
                    return @{ Ok = $true; Reason = 'opened'; Path = $path }
                } finally {
                    [System.Runtime.InteropServices.Marshal]::FreeHGlobal($buf)
                }
            } finally {
                if ($IsInput) { [WinMMDrv]::midiInClose($h)  | Out-Null }
                else          { [WinMMDrv]::midiOutClose($h) | Out-Null }
            }
        }
        # Device-id-as-handle worked. Allocate buffer and fetch the path.
        $buf = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($cbSize)
        try {
            $rc = if ($IsInput) {
                [WinMMDrv]::midiInMessage([IntPtr]$DeviceId, [WinMMDrv]::DRV_QUERYDEVICEINTERFACE, $buf, [IntPtr]$cbSize)
            } else {
                [WinMMDrv]::midiOutMessage([IntPtr]$DeviceId, [WinMMDrv]::DRV_QUERYDEVICEINTERFACE, $buf, [IntPtr]$cbSize)
            }
            if ($rc -ne 0) {
                return @{ Ok = $false; Reason = "interface query rc=$rc"; Path = $null }
            }
            $path = [System.Runtime.InteropServices.Marshal]::PtrToStringUni($buf, [int]($cbSize / 2)).TrimEnd([char]0)
            return @{ Ok = $true; Reason = 'device-id-as-handle'; Path = $path }
        } finally {
            [System.Runtime.InteropServices.Marshal]::FreeHGlobal($buf)
        }
    } finally {
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($sizePtr)
    }
}

function Print-Bridge {
    param($Kind, [int]$Count)
    $isInput = $Kind -eq 'in'
    Write-Host ''
    Write-Host "  -- $Kind --"
    for ($i = 0; $i -lt $Count; $i++) {
        $r = Query-MidiDeviceInterface -DeviceId $i -IsInput $isInput
        if (-not $r.Ok) {
            "    $Kind[$i] <query failed: $($r.Reason)>" | Write-Host -ForegroundColor Yellow
            continue
        }
        $serial = $null
        $note = $null
        if ($r.Path -match 'usb#vid_([0-9a-fA-F]{4})&pid_([0-9a-fA-F]{4})#([^#]+)#') {
            $vid = $matches[1].ToUpper()
            $pidx = $matches[2].ToUpper()
            $serial = $matches[3].ToUpper()
            $note = "VID=$vid PID=$pidx (direct USB path)"
        } elseif ($r.Path -match 'MIDIU_KSA_(\d+)') {
            $ksa = $matches[1]
            $serial = $ksaToSerial[$ksa]
            $note = "via KSA $ksa"
        } elseif ($r.Path -match 'root#media' -or [string]::IsNullOrEmpty($r.Path)) {
            $note = 'non-USB MIDI device (loopMIDI / GS Wavetable / etc.)'
        }
        if ($serial) {
            "    $Kind[$i] -> serial {0}    [{1}]" -f $serial, $note | Write-Host
        } else {
            "    $Kind[$i] (no USB serial)   [{0}]" -f $note | Write-Host
        }
        "          path: $($r.Path)" | Write-Host
    }
}

Print-Bridge -Kind 'in'  -Count $inN
Print-Bridge -Kind 'out' -Count $outN

Write-Host ''
Write-Host '== Done ==' -ForegroundColor Cyan
Write-Host 'If Stage 9 prints a real device-interface path with a MIDIU_KSA hash'
Write-Host 'for every Exquis port, the bridge is solved: midir/winmm device index'
Write-Host '-> DRV_QUERYDEVICEINTERFACE -> KSA hash -> real USB serial. The mapping'
Write-Host 'is read straight from the OS at xentool startup, no WinRT needed.'
