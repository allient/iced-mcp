-- Start empty, add three todos through the input, check the counter and
-- the app's own model, and return what it holds.
app:load_fixture("empty")

for _, text in ipairs({ "milk", "eggs", "bread" }) do
	app:type_text({ id = "new-todo", text = text, submit = true })
end

app:wait_for("3 items, 0 done")
app:expect({ text = "3 items, 0 done", visible = true, fits = true })
app:expect_query({ name = "items", contains = "bread" })

local items = app:query("items")
assert(#items == 3, `expected 3 todos, got {#items}`)
return items
