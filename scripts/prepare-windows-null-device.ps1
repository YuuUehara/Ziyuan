# Windows Server (including GitHub Actions windows-2025) does not grant
# ALL APPLICATION PACKAGES (S-1-15-2-1) access to \Device\Null by default,
# unlike Windows 10/11 Desktop. Align \Device\Null's DACL and Low IL label
# with Windows 11 Desktop defaults so AppContainer child tools (git, cmd >nul)
# can open NUL / /dev/null cleanly.
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class WinNullPrep {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")]
    static extern IntPtr LocalFree(IntPtr h);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern uint GetSecurityInfo(IntPtr h, int objType, uint secInfo, out IntPtr o, out IntPtr g, out IntPtr d, out IntPtr s, out IntPtr sd);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern uint SetSecurityInfo(IntPtr h, int objType, uint secInfo, IntPtr o, IntPtr g, IntPtr d, IntPtr s);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool ConvertSecurityDescriptorToStringSecurityDescriptorW(IntPtr sd, uint rev, uint secInfo, out IntPtr str, out uint len);
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string sddl, uint rev, out IntPtr sd, out uint size);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool GetSecurityDescriptorDacl(IntPtr sd, out bool present, out IntPtr dacl, out bool defaulted);
    [DllImport("advapi32.dll", SetLastError = true)]
    static extern bool GetSecurityDescriptorSacl(IntPtr sd, out bool present, out IntPtr sacl, out bool defaulted);

    public static string Dump() {
        IntPtr h = CreateFileW(@"\\.\NUL", 0x00020000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
        if (h == new IntPtr(-1)) return "CreateFile failed: " + Marshal.GetLastWin32Error();
        try {
            IntPtr o, g, d, s, sd;
            uint info = 0x00000004 | 0x00000010;
            uint rc = GetSecurityInfo(h, 6, info, out o, out g, out d, out s, out sd);
            if (rc != 0) return "GetSecurityInfo failed: " + rc;
            try {
                IntPtr str; uint len;
                if (!ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, 1, info, out str, out len))
                    return "ConvertSD failed: " + Marshal.GetLastWin32Error();
                try { return Marshal.PtrToStringUni(str); }
                finally { LocalFree(str); }
            } finally { LocalFree(sd); }
        } finally { CloseHandle(h); }
    }

    public static void Prepare() {
        const string sddl = "D:(A;;0x1201bf;;;WD)(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;RC)(A;;0x1201bf;;;AC)(A;;0x1201bf;;;S-1-15-2-2)S:(ML;;NW;;;LW)";
        IntPtr sd; uint sz;
        if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, 1, out sd, out sz))
            throw new InvalidOperationException("ConvertStringSD failed: " + Marshal.GetLastWin32Error());
        try {
            bool dp, dd, sp, sd2; IntPtr dacl, sacl;
            if (!GetSecurityDescriptorDacl(sd, out dp, out dacl, out dd) || !dp)
                throw new InvalidOperationException("DACL missing");
            if (!GetSecurityDescriptorSacl(sd, out sp, out sacl, out sd2) || !sp)
                throw new InvalidOperationException("SACL missing");
            // Try DACL + Low IL label first (WRITE_DAC | WRITE_OWNER); fall back to DACL-only if label write is restricted.
            IntPtr h = CreateFileW(@"\\.\NUL", 0x000C0000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
            if (h != new IntPtr(-1)) {
                try {
                    uint rc = SetSecurityInfo(h, 6, 0x00000004 | 0x00000010, IntPtr.Zero, IntPtr.Zero, dacl, sacl);
                    if (rc == 0) return;
                } finally { CloseHandle(h); }
            }
            IntPtr h2 = CreateFileW(@"\\.\NUL", 0x00040000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
            if (h2 == new IntPtr(-1))
                throw new InvalidOperationException("CreateFileW(\\\\.\\NUL, WRITE_DAC) failed: " + Marshal.GetLastWin32Error());
            try {
                uint rc = SetSecurityInfo(h2, 6, 0x00000004, IntPtr.Zero, IntPtr.Zero, dacl, IntPtr.Zero);
                if (rc != 0)
                    throw new InvalidOperationException("SetSecurityInfo(DACL) failed: " + rc);
            } finally { CloseHandle(h2); }
        } finally { LocalFree(sd); }
    }
}
"@

$before = [WinNullPrep]::Dump()
Write-Output "BEFORE: $before"
if ($before.Contains(';;;AC)') -and $before.Contains(';;;LW)')) {
    Write-Output "NUL already grants AppContainer + Low IL; skipping."
    exit 0
}
[WinNullPrep]::Prepare()
$after = [WinNullPrep]::Dump()
Write-Output "AFTER:  $after"
