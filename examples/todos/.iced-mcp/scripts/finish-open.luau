-- A conditional: tick whatever is still open, then everything is done.
app:load_fixture("groceries")

local open = app:query("items", { done = false })
if #open == 0 then
	print("nothing left to do")
else
	for _, item in ipairs(open) do
		app:click({ text = item.text, exact = true })
		print(`ticked {item.text}`)
	end
end

app:wait_for("3 items, 3 done")
local toggles = app:messages({ filter = "Toggle" })
assert(#toggles >= #open, `expected {#open} Toggle messages, saw {#toggles}`)
return app:query("items", { done = false })
