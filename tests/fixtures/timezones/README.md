These public-domain TZif files are compiled from IANA tzdata **2025b**:
https://data.iana.org/time-zones/releases/tzdata2025b.tar.gz

Source archive SHA-256:
`11810413345fc7805017e27ea9fa4885fd74cd61b2911711ad038f5d28d71474`

They were compiled by `zic` (Ubuntu GLIBC 2.43-2ubuntu2.4) 2.43,
`zic -b fat -d <output> europe southamerica`, then copied from that output.
They retain historical transitions and the POSIX footer. Tests load these
bytes through jiff, never a host or bundled time zone database.

| File | SHA-256 |
|---|---|
| Europe/Berlin | `5ee475f71a0fc1a32faeb849f8c39c6e7aa66d6d41ec742b97b3a7436b3b0701` |
| America/Sao_Paulo | `04c06744ee3fe078ef3b8b779e38eb30666bf993e67a092fcaf2fc28f63a64ce` |

The source release's LICENSE states: “Unless specified below, all files
in the tz code and data (including this LICENSE file) are in the public
domain.” Its exceptions concern date.c, newstrftime.3 and strftime.c;
neither fixture derives from those files.
