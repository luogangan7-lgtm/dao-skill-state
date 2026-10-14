The skill was used for this request:

> Clean customers_eu.csv following our team's customer-CSV cleaning rules and write the result to clean.csv.

customers_eu.csv:
```
email,name,signup_date,lifetime_value
dana@example.com,dana white,05/03/2024,10
eli@example.com,eli brown,2024-02-01,20
```

The agent produced clean.csv:
```
email,name,signup_date,lifetime_value_cents
dana@example.com,Dana White,2024-05-03,1000
eli@example.com,Eli Brown,2024-02-01,2000
```

Data owner's review: "This file comes from our EU system, which writes slashed dates day first:
05/03/2024 is the 5th of March, 2024-03-05. The rest is right."
