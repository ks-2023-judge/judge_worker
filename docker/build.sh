#!/bin/bash

# Dynamically find the subdirectories
languages=$(find . -maxdepth 1 -mindepth 1 -type d -printf "%f\n")

# Loop through each language and build the corresponding Docker images
for lang in $languages; do
    # Skip if either Dockerfile doesn't exist
    if [[ ! -f "$lang/Dockerfile_compile" ]] || [[ ! -f "$lang/Dockerfile_run" ]]; then
        echo "Skipping $lang because a Dockerfile is missing."
        continue
    fi

    echo "Building $lang Docker images..."

    # Compile image
    docker build -t "${lang}_compile" -f "$lang/Dockerfile_compile" $lang
    if [ $? -ne 0 ]; then
        echo "Failed to build ${lang}_compile. Exiting."
        exit 1
    fi

    # Run image
    docker build -t "${lang}_run" -f "$lang/Dockerfile_run" $lang
    if [ $? -ne 0 ]; then
        echo "Failed to build ${lang}_run. Exiting."
        exit 1
    fi

    echo "Successfully built $lang Docker images."
done

echo "All Docker images built successfully."
