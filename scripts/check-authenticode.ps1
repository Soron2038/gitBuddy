# Fail unless a file carries an Authenticode signature good enough for the
# SignPath policy it was signed under. Used by .github/workflows/release-windows.yml
# after each signing step, so an unsigned or broken binary never reaches the
# release.
#
#   release-signing: the signature must be valid, i.e. chain to a root Windows
#                    trusts (SignPath Foundation's certificate).
#   test-signing:    any signature will do; SignPath's test certificate isn't
#                    trusted by Windows, so the status can't be Valid.
#
# Usage: scripts/check-authenticode.ps1 <path> <test-signing|release-signing>
param(
    [Parameter(Mandatory)] [string] $Path,
    [Parameter(Mandatory)] [ValidateSet('test-signing', 'release-signing')] [string] $Policy
)
$ErrorActionPreference = 'Stop'

$sig = Get-AuthenticodeSignature -FilePath $Path
if (-not $sig.SignerCertificate) {
    throw "${Path} is not signed ($($sig.Status): $($sig.StatusMessage))."
}
$stamp = if ($sig.TimeStamperCertificate) { $sig.TimeStamperCertificate.Subject } else { 'none' }
"${Path}: $($sig.Status)"
"  signer:    $($sig.SignerCertificate.Subject)"
"  timestamp: $stamp"
if ($Policy -eq 'release-signing' -and $sig.Status -ne 'Valid') {
    throw "${Path}: the signature is $($sig.Status): $($sig.StatusMessage)"
}
if (-not $sig.TimeStamperCertificate) {
    # Without a timestamp the signature stops validating when the certificate
    # expires, which for SignPath's short-lived certificates is soon.
    throw "${Path}: the signature has no timestamp."
}
