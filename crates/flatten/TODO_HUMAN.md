- Gotta double check all the external stuff, specially the handling of transitive deps. Sounds to me like it's a mess.
- Move expand from a different crate to a feature, on by default. I think that would make it easier to use.
- on minify, it's doing it byte by byte, I don't understand why. I think it would be easier and less flaky if it was done by lines. At least check it out
    - maybe even using regex to find what to delete


--- claude session
    
claude --resume f7c7fcd3-0919-43ea-9e88-5f11285b8872 --dangerously-skip-permissions