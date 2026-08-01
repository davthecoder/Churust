-- HTTP/1.1 pipelining for wrk, depth from the PIPELINE environment variable.
--
-- Why this exists: on a loopback interface the per-packet cost is paid by the
-- kernel, not by either process, and on some machines — this one included — it
-- saturates far below what any of these servers can answer. Measured that way
-- every framework reports the same number and the benchmark says nothing.
--
-- Pipelining puts N requests in one segment, so the kernel does a fraction of
-- the work per request and the server's own parse-route-respond path becomes
-- the thing being measured. It is the same reason TechEmpower's plaintext
-- benchmark pipelines. It is not a simulation of browser traffic, and the
-- unpipelined run in run.sh is reported next to it for exactly that reason.
local depth = tonumber(os.getenv("PIPELINE") or "16")

init = function(args)
   local parts = {}
   for i = 1, depth do
      parts[i] = wrk.format("GET", wrk.path)
   end
   pipelined = table.concat(parts)
end

request = function()
   return pipelined
end
