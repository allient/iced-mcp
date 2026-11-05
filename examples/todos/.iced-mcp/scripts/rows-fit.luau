-- Every todo row is on screen, inside the list and fully readable.
app:load_fixture("groceries")

local items = app:query("items")
for _, item in ipairs(items) do
	app:expect({ text = item.text, exact = true, visible = true, fits = true, inside = { id = "todo-list" } })
end

local texts = app:find_all({ role = "text" })
print(`{#items} rows checked; {#texts} text nodes on screen`)
return #items
