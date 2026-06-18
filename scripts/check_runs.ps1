$ErrorActionPreference='Stop'
$tok = $env:GITHUB_TOKEN
if (-not $tok) { Write-Host 'NO_TOKEN'; exit 2 }
$h = @{ Authorization = ('Bearer ' + $tok); Accept = 'application/vnd.github+json'; 'X-GitHub-Api-Version' = '2022-11-28' }
$r = Invoke-RestMethod -Method Get -Uri 'https://api.github.com/repos/mailrangrua1/Augment-BYOK-Proxy/actions/runs?per_page=5' -Headers $h
foreach ($run in $r.workflow_runs) {
  Write-Host ('{0} | {1} | {2} | {3} | {4}' -f $run.id, $run.name, $run.status, $run.conclusion, $run.head_sha.Substring(0,7))
}
